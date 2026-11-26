use super::*;

fn infrastructure_errors() -> [Value; 2] {
    [
        json!({
            "type":"error",
            "error":{"type":"invalid_request_error","code":"rebuild_target_not_ready","message":"upstream is unavailable"}
        }),
        json!({
            "type":"response.failed",
            "response":{"id":"resp-failed","status":"failed","error":{"type":"invalid_request_error","code":"rebuild_target_not_ready","message":"upstream is unavailable"}}
        }),
    ]
}

async fn receive_upstream_request(socket: &mut WebSocketStream<TcpStream>) -> Value {
    let WebSocketMessage::Text(text) = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    else {
        panic!("expected response.create");
    };
    let body: Value = serde_json::from_str(text.as_str()).unwrap();
    assert_eq!(body["type"], "response.create");
    body
}

async fn send_upstream_event(socket: &mut WebSocketStream<TcpStream>, event: Value) {
    socket
        .send(WebSocketMessage::Text(event.to_string().into()))
        .await
        .unwrap();
}

fn completed_event(id: &str) -> Value {
    json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[]}})
}

#[test]
fn websocket_infrastructure_failure_requires_an_exact_structured_code() {
    for event in infrastructure_errors() {
        assert!(responses_event_is_websocket_infrastructure_failure(&event));
    }
    for event in [
        json!({"type":"error","error":{"type":"invalid_request_error","message":"rebuild_target_not_ready"}}),
        json!({"type":"error","error":{"type":"rebuild_target_not_ready","code":"invalid_request_error"}}),
        json!({"type":"response.failed","response":{"error":{"code":"invalid_request_error","message":"rebuild_target_not_ready"}}}),
        json!({"type":"error","error":{"code":"rebuild_target_not_ready_extra"}}),
        json!({"type":"response.completed","response":{"error":{"code":"rebuild_target_not_ready"}}}),
    ] {
        assert!(!responses_event_is_websocket_infrastructure_failure(&event));
    }
}

#[tokio::test]
async fn websocket_infrastructure_error_backs_off_cached_sessions_without_replay() {
    for failure in infrastructure_errors() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut cached_socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert_eq!(
                receive_upstream_request(&mut cached_socket).await["input"],
                "warm"
            );
            send_upstream_event(&mut cached_socket, completed_event("resp-warm")).await;

            let (stream, _) = upstream.accept().await.unwrap();
            let mut failed_socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert_eq!(
                receive_upstream_request(&mut failed_socket).await["input"],
                "fail"
            );
            send_upstream_event(&mut failed_socket, failure).await;

            // 第一条 HTTP 必须是下一次请求，不能重放刚才失败的请求。
            for input in ["same-session", "cached-session"] {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                assert_eq!(request.method, "POST");
                assert_eq!(request.path, "/v1/responses");
                assert_eq!(
                    serde_json::from_slice::<Value>(&request.body).unwrap()["input"],
                    input
                );
                let response =
                    json!({"id":format!("resp-{input}"),"status":"completed","output":[]})
                        .to_string();
                stream
                    .write_all(format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                        response.len()
                    ).as_bytes())
                    .await
                    .unwrap();
            }
            for socket in [&mut cached_socket, &mut failed_socket] {
                let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                    .await
                    .expect("backoff must drop cached upstream sockets");
                assert!(!matches!(message, Some(Ok(WebSocketMessage::Text(_)))));
            }
        });

        let (mut config, provider_id, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let alias = model_alias(&provider_id, &model);
        let mut cached = connect_router_websocket(&router.endpoint()).await;
        let warm = send_router_websocket_request(&mut cached, &alias, "warm").await;
        assert_eq!(warm.last().unwrap()["type"], "response.completed");
        let mut failed = connect_router_websocket(&router.endpoint()).await;
        let failure = send_router_websocket_request(&mut failed, &alias, "fail").await;
        assert_eq!(failure.len(), 1);
        assert_eq!(failure[0]["type"], "response.failed");
        assert_eq!(
            failure[0]["response"]["error"]["codey"]["originalCode"],
            "rebuild_target_not_ready"
        );
        {
            let backoffs = router.websocket_backoffs.lock().unwrap();
            assert_eq!(backoffs.entries.len(), 1);
            let backoff = backoffs.entries.values().next().unwrap();
            assert_eq!(backoff.failure_count, 1);
            assert!(backoff.until > Instant::now());
        }
        for (socket, input) in [
            (&mut failed, "same-session"),
            (&mut cached, "cached-session"),
        ] {
            let events = send_router_websocket_request(socket, &alias, input).await;
            assert_eq!(events.last().unwrap()["type"], "response.completed");
            assert_eq!(
                events.last().unwrap()["response"]["id"],
                format!("resp-{input}")
            );
        }
        assert_eq!(router.websocket_backoffs.lock().unwrap().entries.len(), 1);
        upstream_task.await.unwrap();
        cached.close(None).await.unwrap();
        failed.close(None).await.unwrap();
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn websocket_business_error_keeps_connection_without_backoff() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        receive_upstream_request(&mut socket).await;
        send_upstream_event(&mut socket, json!({
            "type":"error",
            "error":{"type":"invalid_request_error","code":"invalid_request_error","message":"input contains rebuild_target_not_ready"}
        })).await;
        receive_upstream_request(&mut socket).await;
        send_upstream_event(&mut socket, completed_event("resp-valid")).await;
    });
    let (mut config, provider_id, model) = router_config(format!("http://{address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let alias = model_alias(&provider_id, &model);
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    let failed = send_router_websocket_request(&mut socket, &alias, "invalid").await;
    assert_eq!(failed.last().unwrap()["type"], "response.failed");
    assert!(router.websocket_backoffs.lock().unwrap().entries.is_empty());
    let completed = send_router_websocket_request(&mut socket, &alias, "valid").await;
    assert_eq!(completed.last().unwrap()["response"]["id"], "resp-valid");
    upstream_task.await.unwrap();
    socket.close(None).await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn websocket_expired_backoff_survives_handshake_and_errors_until_success() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        receive_upstream_request(&mut socket).await;
        send_upstream_event(
            &mut socket,
            json!({
                "type":"response.failed",
                "response":{"error":{"code":"invalid_request_error","message":"invalid input"}}
            }),
        )
        .await;
        receive_upstream_request(&mut socket).await;
        send_upstream_event(&mut socket, infrastructure_errors()[1].clone()).await;
        let (stream, _) = upstream.accept().await.unwrap();
        let mut recovered = tokio_tungstenite::accept_async(stream).await.unwrap();
        receive_upstream_request(&mut recovered).await;
        send_upstream_event(&mut recovered, completed_event("resp-recovered")).await;
    });
    let (mut config, provider_id, model) = router_config(format!("http://{address}/v1"));
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let route = &snapshot.routes[&provider_id];
    let headers = HeaderMap::new();
    let key = UpstreamWebSocketBackoffKey::for_route(
        route,
        route.upstream_websocket_url.as_ref().unwrap(),
        UpstreamWebSocketAuthIdentity::from_headers(&headers),
    );
    let (socket, mut peer) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(socket);
    let expired_at = Instant::now();
    downstream
        .websocket_backoffs
        .lock()
        .unwrap()
        .record_failure(
            key.clone(),
            expired_at - upstream_websocket_backoff_duration(1),
        );

    for (input, failure_count) in [
        ("invalid", Some(1)),
        ("unavailable", Some(2)),
        ("recovered", None),
    ] {
        downstream.clear_stream_id();
        let mut body = json!({"model":model,"input":input});
        assert_eq!(
            downstream
                .proxy_upstream_websocket(route, &headers, &mut body, false, None)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::Completed
        );
        let WebSocketMessage::Text(text) = peer.next().await.unwrap().unwrap() else {
            panic!("expected a terminal event");
        };
        let event: Value = serde_json::from_str(text.as_str()).unwrap();
        let mut backoffs = downstream.websocket_backoffs.lock().unwrap();
        match failure_count {
            Some(expected) => {
                assert_eq!(event["type"], "response.failed");
                let backoff = backoffs.entries.get_mut(&key).unwrap();
                assert_eq!(backoff.failure_count, expected);
                if expected == 1 {
                    assert_eq!(backoff.until, expired_at);
                    assert!(downstream.upstream.is_some());
                } else {
                    assert!(backoff.until > Instant::now());
                    assert!(downstream.upstream.is_none());
                    // 直接推进记录的到期点，避免等待实际退避时间。
                    backoff.until = Instant::now();
                }
            }
            None => {
                assert_eq!(event["type"], "response.completed");
                assert!(!backoffs.entries.contains_key(&key));
                assert!(downstream.upstream.is_some());
            }
        }
    }
    upstream_task.await.unwrap();
}

#[tokio::test]
async fn websocket_infrastructure_backoff_uses_cached_connection_identity() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        receive_upstream_request(&mut socket).await;
        send_upstream_event(&mut socket, completed_event("resp-original")).await;
        let request = receive_upstream_request(&mut socket).await;
        assert_eq!(request["previous_response_id"], "resp-original");
        send_upstream_event(&mut socket, infrastructure_errors()[0].clone()).await;
    });
    let (mut config, provider_id, model) = router_config(format!("http://{address}/v1"));
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let route = &snapshot.routes[&provider_id];
    let (socket, mut peer) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(socket);
    let mut keys = Vec::new();
    for (token, previous_response) in [
        ("Bearer original", None),
        ("Bearer changed", Some("resp-original")),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static(token));
        keys.push(UpstreamWebSocketBackoffKey::for_route(
            route,
            route.upstream_websocket_url.as_ref().unwrap(),
            UpstreamWebSocketAuthIdentity::from_headers(&headers),
        ));
        let mut body = json!({"model":model,"input":"continue"});
        if let Some(previous_response) = previous_response {
            body["previous_response_id"] = previous_response.into();
        }
        downstream.clear_stream_id();
        assert_eq!(
            downstream
                .proxy_upstream_websocket(route, &headers, &mut body, false, None)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::Completed
        );
        peer.next().await.unwrap().unwrap();
    }
    {
        let backoffs = downstream.websocket_backoffs.lock().unwrap();
        assert!(backoffs.is_backing_off(&keys[0], Instant::now()));
        assert!(!backoffs.entries.contains_key(&keys[1]));
        let mut changed_route = keys[0].clone();
        changed_route.route_id.push_str("-other");
        assert!(!backoffs.is_backing_off(&changed_route, Instant::now()));
        let mut changed_config = keys[0].clone();
        changed_config.config_identity[0] ^= 1;
        assert!(!backoffs.is_backing_off(&changed_config, Instant::now()));
    }
    upstream_task.await.unwrap();
}

#[test]
fn websocket_handshake_timeout_escalates_progressive_backoff_steps() {
    let (mut config, provider_id, _) = router_config("http://127.0.0.1:9/v1".into());
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let route = &snapshot.routes[&provider_id];
    let key = UpstreamWebSocketBackoffKey::for_route(
        route,
        route.upstream_websocket_url.as_ref().unwrap(),
        UpstreamWebSocketAuthIdentity::default(),
    );
    let mut backoffs = UpstreamWebSocketBackoffs::default();
    backoffs.update_routes(&snapshot);

    let mut now = Instant::now();
    for (expected_count, expected_secs) in [(1, 15), (2, 60), (3, 180), (4, 180)] {
        let (count, duration) = backoffs.record_handshake_timeout(key.clone(), now);
        assert_eq!(count, expected_count);
        assert_eq!(duration, Duration::from_secs(expected_secs));
        assert!(backoffs.is_backing_off(&key, now + Duration::from_secs(1)));
        // 模拟用户在退避到期后 30 秒发起下一轮请求，退避档位应继续递进而不重置为首档。
        now += duration + Duration::from_secs(30);
    }

    // 超出重置窗口后重新从 15s 起步；一旦握手并完成一轮请求则立即清空退避。
    now += Duration::from_secs(181);
    let (count, duration) = backoffs.record_handshake_timeout(key.clone(), now);
    assert_eq!(count, 1);
    assert_eq!(duration, Duration::from_secs(15));
    backoffs.record_success(&key);
    assert!(!backoffs.entries.contains_key(&key));

    let io_timeout = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "handshake timed out",
    ))
    .context("连接 Responses WebSocket 上游超时");
    assert!(upstream_websocket_connect_timed_out(&io_timeout));
    let refused = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "refused",
    ));
    assert!(!upstream_websocket_connect_timed_out(&refused));
}

#[tokio::test]
async fn websocket_1009_message_too_big_falls_back_to_http_sse_without_backoff_penalty() {
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        // 第 1 轮：建立并缓存 WS 连接。
        let (stream, _) = upstream.accept().await.unwrap();
        let mut ws1 = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(receive_upstream_request(&mut ws1).await["input"], "warm");
        send_upstream_event(&mut ws1, completed_event("resp-warm")).await;

        // 第 2 轮（带 previous_response_id 的大请求）：上游 WS 返回 1009 MessageTooBig。
        let oversized = receive_upstream_request(&mut ws1).await;
        assert_eq!(oversized["previous_response_id"], "resp-warm");
        ws1.send(WebSocketMessage::Close(Some(CloseFrame {
            code: CloseCode::Size,
            reason: "Message too big".into(),
        })))
        .await
        .unwrap();

        // 同轮应立即干净回退到 HTTP SSE（展开完整历史且无 type 字段）。
        let (mut http_stream, _) = upstream.accept().await.unwrap();
        let http_req = read_http_request(&mut http_stream).await.unwrap();
        assert_eq!(http_req.method, "POST");
        assert_eq!(http_req.path, "/v1/responses");
        let fallback_body: Value = serde_json::from_slice(&http_req.body).unwrap();
        assert!(fallback_body.get("type").is_none(), "{fallback_body}");
        assert!(fallback_body.get("previous_response_id").is_none());
        let sse = format!("data: {}\n\n", completed_event("resp-http-fallback"));
        http_stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                    sse.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        // 第 3 轮：由于 1009 未惩罚线路 WS 可用性，下一轮直接恢复走 WS！
        let (stream, _) = upstream.accept().await.unwrap();
        let mut ws2 = tokio_tungstenite::accept_async(stream).await.unwrap();
        let resumed = receive_upstream_request(&mut ws2).await;
        assert!(resumed.get("previous_response_id").is_none());
        send_upstream_event(&mut ws2, completed_event("resp-ws-resumed")).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), ws2.next()).await;
    });

    let (mut config, provider_id, model) = router_config(format!("http://{address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let alias = model_alias(&provider_id, &model);
    let mut client = connect_router_websocket(&router.endpoint()).await;

    let first = send_router_websocket_request(&mut client, &alias, "warm").await;
    assert_eq!(first.last().unwrap()["response"]["id"], "resp-warm");

    client
        .send(WebSocketMessage::Text(
            json!({
                "type": "response.create",
                "model": alias,
                "previous_response_id": "resp-warm",
                "input": [{"role": "user", "content": "huge-turn"}],
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let WebSocketMessage::Text(second_text) =
        tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    else {
        panic!("expected fallback completion");
    };
    let second_event: Value = serde_json::from_str(&second_text).unwrap();
    assert_eq!(second_event["type"], "response.completed");
    assert_eq!(second_event["response"]["id"], "resp-http-fallback");
    assert!(
        router.websocket_backoffs.lock().unwrap().entries.is_empty(),
        "1009 MessageTooBig must not penalize route WebSocket availability"
    );

    client
        .send(WebSocketMessage::Text(
            json!({
                "type": "response.create",
                "model": alias,
                "previous_response_id": "resp-http-fallback",
                "input": [{"role": "user", "content": "normal-turn"}],
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let WebSocketMessage::Text(third_text) =
        tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    else {
        panic!("expected resumed ws completion");
    };
    let third_event: Value = serde_json::from_str(&third_text).unwrap();
    assert_eq!(third_event["response"]["id"], "resp-ws-resumed");

    client.close(None).await.unwrap();
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}
