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
