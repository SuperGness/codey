use super::*;

const THREAD: &str = "interrupt-test";

async fn fixture() -> (LocalRouter, TcpListener, String) {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider, model) =
        super::super::tests::router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    config.profiles[0].supports_remote_compaction = true;
    config.profiles[0].remote_compaction_protocol =
        crate::config::RemoteCompactionProtocol::CompactEndpoint;
    let router = LocalRouter::start(&config).await.unwrap();
    (router, upstream, model_alias(&provider, &model))
}

async fn compact_request(router: &LocalRouter, model: &str) -> TcpStream {
    let endpoint = router.endpoint();
    let url = reqwest::Url::parse(&endpoint.base_url).unwrap();
    let mut client = TcpStream::connect((url.host_str().unwrap(), url.port().unwrap()))
        .await
        .unwrap();
    let body = serde_json::to_vec(&json!({"model":model,"input":"context"})).unwrap();
    let headers = format!(
        "POST {}/responses/compact HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nthread-id: {THREAD}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        url.path().trim_end_matches('/'),
        endpoint.token,
        body.len()
    );
    client.write_all(headers.as_bytes()).await.unwrap();
    client.write_all(&body).await.unwrap();
    client
}

fn result() -> Value {
    json!({"object":"response.compaction","output":[
        {"type":"compaction","encrypted_content":"opaque-interrupt-test"}
    ]})
}

async fn held_upstream(upstream: &TcpListener, body_wait: bool) -> TcpStream {
    let (mut socket, _) = upstream.accept().await.unwrap();
    let request = read_http_request(&mut socket).await.unwrap();
    assert!(request.path.ends_with("/responses/compact"));
    if body_wait {
        let body = serde_json::to_vec(&result()).unwrap();
        socket.write_all(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ).as_bytes()).await.unwrap();
        socket.write_all(&body[..1]).await.unwrap();
        socket.flush().await.unwrap();
    }
    socket
}

async fn assert_released(router: &LocalRouter) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let released = {
                let bindings = router.bindings.lock().unwrap();
                !bindings.compacting.contains(&format!("thread-id:{THREAD}"))
                    && bindings.compaction_operations.is_empty()
            };
            if released {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("compaction must release without an upstream response");
    assert!(
        router
            .bindings
            .lock()
            .unwrap()
            .compaction_operations
            .is_empty()
    );
}

async fn assert_upstream_cancelled(socket: &mut TcpStream) {
    let mut byte = [0; 1];
    let read = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
        .await
        .expect("cancel must close the pending upstream request");
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "upstream request remained active: {read:?}"
    );
}

#[tokio::test]
async fn interrupt_cancels_header_and_body_waits_after_fin_and_allows_resend() {
    for body_wait in [false, true] {
        for fin in [false, true] {
            let (router, upstream, model) = fixture().await;
            let client = compact_request(&router, &model).await;
            let mut socket = held_upstream(&upstream, body_wait).await;
            let mut client = Some(client);
            if fin {
                drop(client.take());
            }
            assert_eq!(
                router
                    .interrupt_portable_compaction(&json!({"threadId":THREAD}))
                    .await
                    .unwrap(),
                json!({"status":"ok"})
            );
            assert_released(&router).await;
            assert_upstream_cancelled(&mut socket).await;
            drop(client);

            let next = compact_request(&router, &model).await;
            let (mut socket, _) = upstream.accept().await.unwrap();
            read_http_request(&mut socket).await.unwrap();
            write_json_response(&mut socket, 200, &result())
                .await
                .unwrap();
            drop(socket);
            let mut next = next;
            let mut response = Vec::new();
            tokio::time::timeout(Duration::from_secs(3), next.read_to_end(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200"));
            assert_released(&router).await;
            router.stop().await.unwrap();
        }
    }
}

#[tokio::test]
async fn reset_cancels_header_and_body_waits_after_fin_or_pipelined_bytes() {
    for body_wait in [false, true] {
        for prefix in ["none", "fin", "pipelined"] {
            let (router, upstream, model) = fixture().await;
            let mut client = compact_request(&router, &model).await;
            let mut socket = held_upstream(&upstream, body_wait).await;
            match prefix {
                "fin" => client.shutdown().await.unwrap(),
                "pipelined" => client.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap(),
                _ => {}
            }
            // 先让监听处理 FIN 或已到达的字节，再单独发送 RST。
            tokio::time::sleep(Duration::from_millis(75)).await;
            socket2::SockRef::from(&client)
                .set_linger(Some(Duration::ZERO))
                .unwrap();
            drop(client);
            assert_released(&router).await;
            assert_upstream_cancelled(&mut socket).await;
            router.stop().await.unwrap();
        }
    }
}

#[tokio::test]
async fn write_half_close_preserves_header_and_body_waits() {
    for body_wait in [false, true] {
        let (router, upstream, model) = fixture().await;
        let mut client = compact_request(&router, &model).await;
        let mut socket = held_upstream(&upstream, body_wait).await;
        client.shutdown().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            router
                .bindings
                .lock()
                .unwrap()
                .compacting
                .contains(&format!("thread-id:{THREAD}")),
            "a legal write half-close must keep waiting for the response"
        );
        let body = serde_json::to_vec(&result()).unwrap();
        if body_wait {
            socket.write_all(&body[1..]).await.unwrap();
            socket.flush().await.unwrap();
        } else {
            write_json_response(&mut socket, 200, &result())
                .await
                .unwrap();
        }
        drop(socket);
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert_released(&router).await;
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn interrupt_is_idempotent_scoped_and_does_not_cancel_a_later_generation() {
    let (router, _upstream, _model) = fixture().await;
    let payload = json!({"threadId":THREAD});
    assert_eq!(
        router
            .interrupt_portable_compaction(&payload)
            .await
            .unwrap()["status"],
        "ok"
    );
    assert!(
        router
            .interrupt_portable_compaction(&json!({"threadId":"\n"}))
            .await
            .is_err()
    );
    let keys = vec![format!("thread-id:{THREAD}")];
    let old = CompactionGuard::acquire(&router.bindings, keys.clone()).unwrap();
    let child = CompactionGuard::acquire(
        &router.bindings,
        vec!["thread-id:child".into(), format!("session-id:{THREAD}")],
    )
    .unwrap();
    let operations = router
        .bindings
        .lock()
        .unwrap()
        .compaction_operations
        .clone();
    let stopping = router.interrupt_portable_compaction(&payload);
    tokio::pin!(stopping);
    assert!(futures_util::poll!(&mut stopping).is_pending());
    for operation in operations.values() {
        assert_eq!(
            *operation.cancel.borrow(),
            operation.thread_key.as_deref() == Some(&keys[0])
        );
    }
    drop(old);
    let next = CompactionGuard::acquire(&router.bindings, keys.clone()).unwrap();
    assert_eq!(stopping.await.unwrap()["status"], "ok");
    assert!(
        router
            .bindings
            .lock()
            .unwrap()
            .compacting
            .contains(&keys[0])
    );
    for operation in router
        .bindings
        .lock()
        .unwrap()
        .compaction_operations
        .values()
    {
        assert!(!*operation.cancel.borrow());
    }
    drop(next);
    drop(child);
    assert_eq!(
        router
            .interrupt_portable_compaction(&payload)
            .await
            .unwrap()["status"],
        "ok"
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn interrupt_timeout_keeps_exclusion_until_the_operation_finishes() {
    let (router, _upstream, _model) = fixture().await;
    let keys = vec![format!("thread-id:{THREAD}")];
    let guard = CompactionGuard::acquire(&router.bindings, keys.clone()).unwrap();
    tokio::time::pause();
    let error = router
        .interrupt_portable_compaction(&json!({"threadId":THREAD}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("取消超时"));
    assert!(
        router
            .bindings
            .lock()
            .unwrap()
            .compacting
            .contains(&keys[0])
    );
    drop(guard);
    assert_released(&router).await;
    router.stop().await.unwrap();
}

#[tokio::test]
async fn interrupt_cancels_an_incomplete_upstream_upload() {
    let (router, upstream, model) = fixture().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let request = client
        .post(format!("{}/responses/compact", router.endpoint.base_url))
        .bearer_auth(&router.endpoint.token)
        .header("thread-id", THREAD)
        .json(&json!({"model":model,"input":"x".repeat(16 * 1024 * 1024)}))
        .send();
    let pending = tokio::spawn(request);
    let (mut socket, _) = upstream.accept().await.unwrap();
    socket2::SockRef::from(&socket)
        .set_recv_buffer_size(1024)
        .unwrap();
    let mut prefix = [0; 1024];
    assert!(socket.read(&mut prefix).await.unwrap() > 0);
    router
        .interrupt_portable_compaction(&json!({"threadId":THREAD}))
        .await
        .unwrap();
    assert_released(&router).await;
    // 取消时上游尚未收完正文；读出已缓冲的字节后应结束连接。
    let mut received = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut received))
        .await
        .expect("cancel must stop uploading without an upstream response");
    assert!(
        read.is_ok()
            || read.is_err_and(|error| matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ))
    );
    assert!(received.len() < 16 * 1024 * 1024);
    assert!(pending.await.unwrap().is_err());
    router.stop().await.unwrap();
}

#[tokio::test]
async fn interrupt_leaves_another_active_thread_with_a_shared_session_running() {
    let (router, upstream, model) = fixture().await;
    let _client = compact_request(&router, &model).await;
    let mut stopped = held_upstream(&upstream, false).await;
    let client = reqwest::Client::new();
    let other = tokio::spawn(
        client
            .post(format!("{}/responses/compact", router.endpoint.base_url))
            .bearer_auth(&router.endpoint.token)
            .header("thread-id", "other")
            .header("session-id", THREAD)
            .json(&json!({"model":model,"input":"other context"}))
            .send(),
    );
    let mut unaffected = held_upstream(&upstream, false).await;
    router
        .interrupt_portable_compaction(&json!({"threadId":THREAD}))
        .await
        .unwrap();
    {
        let bindings = router.bindings.lock().unwrap();
        assert!(!bindings.compacting.contains(&format!("thread-id:{THREAD}")));
        assert!(bindings.compacting.contains("thread-id:other"));
        assert!(
            bindings
                .compacting
                .contains(&format!("session-id:{THREAD}"))
        );
    }
    assert_upstream_cancelled(&mut stopped).await;
    write_json_response(&mut unaffected, 200, &result())
        .await
        .unwrap();
    assert_eq!(
        other.await.unwrap().unwrap().status(),
        reqwest::StatusCode::OK
    );
    assert_released(&router).await;
    router.stop().await.unwrap();
}

async fn websocket_event(client: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(3), client.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let WebSocketMessage::Text(text) = message else {
        panic!("expected a response event, received {message:?}");
    };
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn interrupt_preserves_queued_websocket_requests_and_connection_reuse() {
    for body_wait in [false, true] {
        let (router, upstream, model) = fixture().await;
        let mut client = super::super::tests::connect_router_websocket_with_headers(
            &router.endpoint(),
            &[("thread-id", THREAD)],
        )
        .await;
        client
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.create", "model":model,
                    "stream_id":"compaction-stream",
                    "input":[{"role":"user","content":"context"},{"type":"compaction_trigger"}]
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let mut socket = held_upstream(&upstream, body_wait).await;
        client
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.create", "model":model,
                    "stream_id":"queued-stream", "input":"queued"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        // Pong 证明前一条 create 已被压缩等待循环读入队列。
        client
            .send(WebSocketMessage::Ping(vec![1, 2, 3].into()))
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), client.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            WebSocketMessage::Pong(vec![1, 2, 3].into())
        );
        router
            .interrupt_portable_compaction(&json!({"threadId":THREAD}))
            .await
            .unwrap();
        assert_released(&router).await;
        assert_upstream_cancelled(&mut socket).await;
        let cancelled = websocket_event(&mut client).await;
        assert_eq!(cancelled["type"], "response.incomplete");
        assert_eq!(cancelled["stream_id"], "compaction-stream");
        assert_eq!(cancelled["response"]["status"], "incomplete");
        assert_eq!(
            cancelled["response"]["incomplete_details"]["reason"],
            "interrupted"
        );
        assert_eq!(cancelled["response"]["output"], json!([]));

        for (input, stream_id) in [("queued", "queued-stream"), ("reused", "reused-stream")] {
            if input == "reused" {
                client
                    .send(WebSocketMessage::Text(
                        json!({
                            "type":"response.create", "model":model,
                            "stream_id":stream_id, "input":input
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            let (mut next, _) = tokio::time::timeout(Duration::from_secs(3), upstream.accept())
                .await
                .expect("the queued request must still reach the upstream")
                .unwrap();
            let request = read_http_request(&mut next).await.unwrap();
            assert!(request.path.ends_with("/responses"));
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["input"], input);
            let response_id = format!("resp-{input}");
            write_json_response(
                &mut next,
                200,
                &json!({
                    "id":response_id, "object":"response", "status":"completed", "output":[]
                }),
            )
            .await
            .unwrap();
            drop(next);
            loop {
                let event = websocket_event(&mut client).await;
                assert_eq!(event["stream_id"], stream_id);
                assert!(
                    !responses_event_is_failure(&event),
                    "unexpected failure: {event}"
                );
                if responses_event_is_terminal(&event) {
                    assert_eq!(event["type"], "response.completed");
                    assert_eq!(event["response"]["id"], response_id);
                    break;
                }
            }
        }
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
    }
}

async fn portable_delivery_fixture() -> (LocalRouter, TcpListener, String, Value) {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, source, model) =
        super::super::tests::router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    let mut target = config.profiles[0].clone();
    target.id = "delivery-target".into();
    let target_id = target.provider_id().to_string();
    config.profiles.push(target);
    config
        .selected_models_by_provider
        .insert(target_id.clone(), vec!["target-model".into()]);
    let router = LocalRouter::start(&config).await.unwrap();
    let resolved = router
        .snapshot
        .read()
        .unwrap()
        .target_for_request(&model, Some(&source), None)
        .unwrap();
    router.bindings.lock().unwrap().remember_resolved(
        &[format!("thread-id:{THREAD}")],
        &resolved,
        false,
        false,
        None,
    );
    let prepared = router
        .portable_compaction(
            "prepare",
            &json!({"threadId":THREAD,"targetProviderId":target_id,"targetModel":"target-model"}),
        )
        .unwrap();
    let handle = json!({"threadId":THREAD,"token":prepared["token"]});
    (router, upstream, model_alias(&source, &model), handle)
}

async fn start_backpressured_portable_delivery(
    router: &LocalRouter,
    upstream: &TcpListener,
    model: &str,
) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
    let mut client = super::super::tests::connect_router_websocket_with_headers(
        &router.endpoint(),
        &[("thread-id", THREAD)],
    )
    .await;
    client
        .send(WebSocketMessage::Text(
            json!({
                "type":"response.create", "model":model, "stream_id":"delivery-stream",
                "input":[{"role":"user","content":"context"},{"type":"compaction_trigger"}]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let (mut socket, _) = upstream.accept().await.unwrap();
    assert!(
        read_http_request(&mut socket)
            .await
            .unwrap()
            .path
            .ends_with("/responses")
    );
    write_json_response(
        &mut socket,
        200,
        &json!({
            "id":"upstream-summary", "object":"response", "status":"completed",
            "output":[{"type":"message","role":"assistant","content":[{
                "type":"output_text","text":"x".repeat(6 * 1024 * 1024)
            }]}]
        }),
    )
    .await
    .unwrap();
    drop(socket);
    let created = websocket_event(&mut client).await;
    assert_eq!(created["type"], "response.created");
    assert_eq!(created["stream_id"], "delivery-stream");
    // 后续三个事件各含约 8 MiB 摘要；暂停读取，确保实际 socket 写入受阻。
    tokio::time::sleep(Duration::from_millis(200)).await;
    client
}

#[tokio::test]
async fn interrupt_during_backpressured_delivery_commits_ticket_and_reuses_websocket() {
    let (router, upstream, model, handle) = portable_delivery_fixture().await;
    let mut client = start_backpressured_portable_delivery(&router, &upstream, &model).await;
    let payload = json!({"threadId":THREAD});
    let stopping = router.interrupt_portable_compaction(&payload);
    tokio::pin!(stopping);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut stopping)
            .await
            .is_err()
    );
    assert_eq!(
        router.portable_compaction("status", &handle).unwrap()["state"],
        "running"
    );
    assert!(
        router
            .bindings
            .lock()
            .unwrap()
            .compacting
            .contains(&format!("thread-id:{THREAD}"))
    );
    let mut terminal_count = 0;
    loop {
        let event = websocket_event(&mut client).await;
        assert_eq!(event["stream_id"], "delivery-stream");
        assert!(!responses_event_is_failure(&event));
        if responses_event_is_terminal(&event) {
            terminal_count += 1;
            assert_eq!(event["type"], "response.completed");
            break;
        }
    }
    assert_eq!(terminal_count, 1);
    assert_eq!(stopping.await.unwrap()["status"], "ok");
    assert_released(&router).await;
    assert_eq!(
        router.portable_compaction("status", &handle).unwrap()["state"],
        "completed"
    );

    client
        .send(WebSocketMessage::Text(
            json!({
                "type":"response.create", "model":model,
                "stream_id":"after-delivery", "input":"next"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let (mut next, _) = tokio::time::timeout(Duration::from_secs(3), upstream.accept())
        .await
        .unwrap()
        .unwrap();
    read_http_request(&mut next).await.unwrap();
    write_json_response(
        &mut next,
        200,
        &json!({
            "id":"resp-after-delivery", "object":"response", "status":"completed", "output":[]
        }),
    )
    .await
    .unwrap();
    loop {
        let event = websocket_event(&mut client).await;
        assert_eq!(event["stream_id"], "after-delivery");
        if responses_event_is_terminal(&event) {
            assert_eq!(event["type"], "response.completed");
            assert_eq!(event["response"]["id"], "resp-after-delivery");
            break;
        }
    }
    client.close(None).await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn interrupt_delivery_deadline_closes_socket_and_fails_ticket_without_a_terminal() {
    let (router, upstream, model, handle) = portable_delivery_fixture().await;
    let mut client = start_backpressured_portable_delivery(&router, &upstream, &model).await;
    router
        .interrupt_portable_compaction(&json!({"threadId":THREAD}))
        .await
        .unwrap();
    assert_released(&router).await;
    assert_eq!(
        router.portable_compaction("status", &handle).unwrap()["state"],
        "failed"
    );
    // 继续读取只能获得关闭或残缺帧错误，不能把旧缓冲区补发为成功终态。
    loop {
        match tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .unwrap()
        {
            Some(Ok(WebSocketMessage::Text(text))) => {
                let event: Value = serde_json::from_str(&text).unwrap();
                assert!(
                    !responses_event_is_terminal(&event),
                    "unexpected terminal: {}",
                    event["type"]
                );
            }
            Some(Ok(WebSocketMessage::Close(_))) | Some(Err(_)) | None => break,
            other => panic!("unexpected message: {other:?}"),
        }
    }
    router.stop().await.unwrap();
}

#[tokio::test]
async fn interrupt_after_http_delivery_starts_finishes_the_same_response() {
    let (router, upstream, model) = fixture().await;
    let mut client = compact_request(&router, &model).await;
    let (mut socket, _) = upstream.accept().await.unwrap();
    read_http_request(&mut socket).await.unwrap();
    let mut value = result();
    value["output"][0]["encrypted_content"] = json!("x".repeat(8 * 1024 * 1024));
    write_json_response(&mut socket, 200, &value).await.unwrap();
    drop(socket);
    let mut prefix = vec![0; 1024];
    let count = client.read(&mut prefix).await.unwrap();
    prefix.truncate(count);
    assert!(prefix.starts_with(b"HTTP/1.1 200"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let payload = json!({"threadId":THREAD});
    let stopping = router.interrupt_portable_compaction(&payload);
    tokio::pin!(stopping);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut stopping)
            .await
            .is_err()
    );
    assert!(
        !router
            .bindings
            .lock()
            .unwrap()
            .routes
            .contains_key(&format!("thread-id:{THREAD}"))
    );
    tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut prefix))
        .await
        .unwrap()
        .unwrap();
    let body_at = prefix
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert_eq!(
        serde_json::from_slice::<Value>(&prefix[body_at..]).unwrap(),
        value
    );
    assert_eq!(stopping.await.unwrap()["status"], "ok");
    assert_released(&router).await;
    assert!(!router.bindings.lock().unwrap().routes[&format!("thread-id:{THREAD}")].portable);
    router.stop().await.unwrap();
}

#[tokio::test]
async fn interrupt_http_delivery_deadline_keeps_native_binding_and_checkpoint_uncommitted() {
    let (router, upstream, model) = fixture().await;
    let mut client = compact_request(&router, &model).await;
    socket2::SockRef::from(&client)
        .set_recv_buffer_size(1024)
        .unwrap();
    let (mut socket, _) = upstream.accept().await.unwrap();
    read_http_request(&mut socket).await.unwrap();
    let mut value = result();
    value["output"][0]["encrypted_content"] = json!("x".repeat(2 * 1024 * 1024));
    write_json_response(&mut socket, 200, &value).await.unwrap();
    drop(socket);
    let mut received = vec![0; 1024];
    let count = client.read(&mut received).await.unwrap();
    received.truncate(count);
    assert!(received.starts_with(b"HTTP/1.1 200"));
    let resolved = router
        .snapshot
        .read()
        .unwrap()
        .target_for_model(&model)
        .unwrap();
    let key = format!("thread-id:{THREAD}");
    let continuation = json!({"model":model, "input":value["output"]});
    // 先证明这个响应能够建立检查点，避免因缓存预算或输入形态产生假阳性。
    let mut control = PortableHistoryCache::default();
    let checkpoint = control
        .capture(
            &key,
            &resolved.route,
            &resolved.upstream_model,
            &json!({"model":model,"input":"context"}),
        )
        .unwrap();
    control.remember(checkpoint, &value);
    assert!(
        control
            .restore(
                &key,
                &resolved.route,
                &resolved.upstream_model,
                &continuation
            )
            .is_some()
    );
    let cache = router.portable_history_cache.upgrade().unwrap();
    assert!(
        cache
            .lock()
            .unwrap()
            .restore(
                &key,
                &resolved.route,
                &resolved.upstream_model,
                &continuation
            )
            .is_none()
    );
    assert!(!router.bindings.lock().unwrap().routes.contains_key(&key));

    router
        .interrupt_portable_compaction(&json!({"threadId":THREAD}))
        .await
        .unwrap();
    assert_released(&router).await;
    assert!(!router.bindings.lock().unwrap().routes.contains_key(&key));
    assert!(
        cache
            .lock()
            .unwrap()
            .restore(
                &key,
                &resolved.route,
                &resolved.upstream_model,
                &continuation
            )
            .is_none()
    );
    socket2::SockRef::from(&client)
        .set_recv_buffer_size(256 * 1024)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    let body_at = received
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert!(serde_json::from_slice::<Value>(&received[body_at..]).is_err());
    router.stop().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn compaction_delivery_without_cancel_keeps_the_normal_send_timeout() {
    let (_cancel, receiver) = tokio::sync::watch::channel(false);
    let started = tokio::time::Instant::now();
    await_compaction_operation(receiver, async {
        begin_compaction_delivery();
        tokio::time::sleep(COMPACTION_DELIVERY_TIMEOUT * 2).await;
        begin_compaction_delivery();
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(started.elapsed(), COMPACTION_DELIVERY_TIMEOUT * 2);
}

#[tokio::test(start_paused = true)]
async fn compaction_delivery_has_one_deadline_for_all_writes_and_state_commit() {
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let second_write = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let started = tokio::time::Instant::now();
    let result = await_compaction_operation(receiver, async {
        begin_compaction_delivery();
        tokio::time::sleep(COMPACTION_DELIVERY_TIMEOUT * 2).await;
        cancel.send_replace(true);
        tokio::task::yield_now().await;
        tokio::time::sleep(COMPACTION_DELIVERY_TIMEOUT / 2).await;
        begin_compaction_delivery();
        second_write.store(true, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(COMPACTION_DELIVERY_TIMEOUT).await;
        completed.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert!(result.unwrap_err().is::<DownstreamClosed>());
    assert_eq!(started.elapsed(), COMPACTION_DELIVERY_TIMEOUT * 3);
    assert!(second_write.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!completed.load(std::sync::atomic::Ordering::SeqCst));
}

struct ControlledSseDownstream {
    inner: HttpResponsesDownstream,
    stage: &'static str,
    reached: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ControlledSseDownstream {
    async fn stop_at_boundary(&mut self) -> Result<()> {
        self.reached.take().unwrap().send(()).unwrap();
        if self.stage.ends_with("blocked") {
            std::future::pending().await
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into())
        }
    }
}

#[async_trait]
impl ResponsesDownstream for ControlledSseDownstream {
    fn event_stream_started(&self) -> bool {
        self.inner.event_stream_started()
    }

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        self.inner.write_error(status, code, message, route).await
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        self.inner.write_json(status, value).await
    }

    async fn start_event_stream(&mut self) -> Result<()> {
        self.inner.start_event_stream().await
    }

    async fn write_event(&mut self, event: &Value) -> Result<()> {
        if self.stage.starts_with("terminal") && responses_event_is_terminal(event) {
            return self.stop_at_boundary().await;
        }
        self.inner.write_event(event).await
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        assert!(self.stage.starts_with("finish"));
        self.stop_at_boundary().await
    }

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        self.inner.proxy_response_with_probe(response, probe).await
    }
}

async fn check_http_sse_commit_boundary(native: bool, stage: &'static str) {
    let (router, upstream, model, handle) = if native {
        let (router, upstream, model) = fixture().await;
        (router, upstream, model, None)
    } else {
        let (router, upstream, model, handle) = portable_delivery_fixture().await;
        (router, upstream, model, Some(handle))
    };
    let (config, _, _) = super::super::tests::router_config("http://127.0.0.1:9/v1".into());
    let mut server = super::super::lifecycle_tests::test_server(&config);
    server.snapshot = Arc::clone(&router.snapshot);
    server.bindings = Arc::clone(&router.bindings);
    server.portable_history_cache = router.portable_history_cache.upgrade().unwrap();
    let server = Arc::new(server);
    let resolved = server
        .snapshot
        .read()
        .unwrap()
        .target_for_model(&model)
        .unwrap();
    let key = format!("thread-id:{THREAD}");
    let original_generation = server
        .bindings
        .lock()
        .unwrap()
        .routes
        .get(&key)
        .map(|route| route.generation);
    let body = json!({"model":model,"stream":true,"input":[
        {"role":"user","content":"context"},{"type":"compaction_trigger"}
    ]});
    let value = if native {
        result()
    } else {
        json!({"id":"summary","object":"response","status":"completed","output":[
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"summary"}]}
        ]})
    };
    let continuation = json!({"model":model,"input":value["output"]});
    if native {
        let mut control = PortableHistoryCache::default();
        let checkpoint = control
            .capture(&key, &resolved.route, &resolved.upstream_model, &body)
            .unwrap();
        control.remember(checkpoint, &value);
        assert!(
            control
                .restore(
                    &key,
                    &resolved.route,
                    &resolved.upstream_model,
                    &continuation
                )
                .is_some()
        );
    }

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (client, socket) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept()
    );
    let (socket, _) = socket.unwrap();
    let mut client = client.unwrap();
    let reading = tokio::spawn(async move {
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let (reached, boundary) = tokio::sync::oneshot::channel();
    let probe = RouteRequestLogProbe::detached_test_probe();
    let working_probe = probe.clone();
    let working_server = Arc::clone(&server);
    let pending = tokio::spawn(async move {
        let _log_guard = RouteRequestLogGuard::new(Some(working_probe.clone()));
        let mut downstream = ControlledSseDownstream {
            inner: HttpResponsesDownstream::new(socket),
            stage,
            reached: Some(reached),
        };
        let mut observed = ObservedResponsesDownstream::new(&mut downstream, Some(working_probe));
        working_server
            .proxy_with_compaction_budget(
                HttpRequest {
                    method: "POST".into(),
                    path: "/v1/responses".into(),
                    headers: vec![("thread-id".into(), THREAD.into())],
                    body: vec![],
                    _body_budget_permit: None,
                },
                body,
                None,
                ResponsesRequestKind::Create,
                &mut observed,
            )
            .await
    });
    let (mut socket, _) = upstream.accept().await.unwrap();
    let request = read_http_request(&mut socket).await.unwrap();
    assert_eq!(request.path.ends_with("/responses/compact"), native);
    write_json_response(&mut socket, 200, &value).await.unwrap();
    drop(socket);
    boundary.await.unwrap();
    let delivered = stage.starts_with("finish");
    // 收尾被阻塞时也必须已经完成提交；终态尚未写出时则必须仍未提交。
    if let Some(handle) = &handle {
        assert_eq!(
            router.portable_compaction("status", handle).unwrap()["state"],
            if delivered {
                "completed"
            } else if stage.ends_with("blocked") {
                "running"
            } else {
                "failed"
            }
        );
    }
    if native {
        assert_eq!(
            server.bindings.lock().unwrap().routes.contains_key(&key),
            delivered
        );
        assert_eq!(
            server
                .portable_history_cache
                .lock()
                .unwrap()
                .restore(
                    &key,
                    &resolved.route,
                    &resolved.upstream_model,
                    &continuation,
                )
                .is_some(),
            delivered
        );
    }
    if stage.ends_with("blocked") {
        tokio::time::pause();
        let stopped_at = tokio::time::Instant::now();
        router
            .interrupt_portable_compaction(&json!({"threadId":THREAD}))
            .await
            .unwrap();
        assert!(stopped_at.elapsed() >= COMPACTION_DELIVERY_TIMEOUT);
        assert!(stopped_at.elapsed() <= COMPACTION_DELIVERY_TIMEOUT + Duration::from_millis(1));
        tokio::time::resume();
    }
    let result = pending.await.unwrap();
    assert_eq!(
        result.is_ok(),
        delivered,
        "native={native}, stage={stage}: {result:?}"
    );
    assert_released(&router).await;
    if let Some(handle) = &handle {
        assert_eq!(
            router.portable_compaction("status", handle).unwrap()["state"],
            if delivered { "completed" } else { "failed" }
        );
        // 可移植票据完成不会提前确认线路切换。
        assert_eq!(
            server.bindings.lock().unwrap().routes[&key].generation,
            original_generation.unwrap()
        );
    }
    if native {
        assert_eq!(
            server.bindings.lock().unwrap().routes.contains_key(&key),
            delivered
        );
        assert_eq!(
            server
                .portable_history_cache
                .lock()
                .unwrap()
                .restore(
                    &key,
                    &resolved.route,
                    &resolved.upstream_model,
                    &continuation,
                )
                .is_some(),
            delivered
        );
    }
    let received = reading.await.unwrap();
    assert!(received.starts_with("HTTP/1.1 200"));
    assert_eq!(
        received.matches("event: response.completed\n").count(),
        usize::from(delivered)
    );
    assert!(!received.ends_with("0\r\n\r\n"));
    router.stop().await.unwrap();
}

#[tokio::test]
async fn http_sse_compaction_finish_failure_preserves_ticket_checkpoint_and_binding() {
    for native in [false, true] {
        check_http_sse_commit_boundary(native, "finish_error").await;
    }
}

#[tokio::test]
async fn http_sse_compaction_finish_deadline_preserves_ticket_checkpoint_and_binding() {
    for native in [false, true] {
        check_http_sse_commit_boundary(native, "finish_blocked").await;
    }
}

#[tokio::test]
async fn http_sse_compaction_unwritten_terminal_keeps_ticket_checkpoint_and_binding_uncommitted() {
    for native in [false, true] {
        for stage in ["terminal_error", "terminal_blocked"] {
            check_http_sse_commit_boundary(native, stage).await;
        }
    }
}
