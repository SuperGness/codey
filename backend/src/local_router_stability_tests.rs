use super::tests::{connect_router_websocket, local_websocket_pair, router_config};
use super::*;

#[test]
fn sse_eof_requires_semantic_completion_but_not_a_transport_marker() {
    let chat = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n";
    let anthropic = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n";
    assert!(parse_chat_completion_sse_bytes(chat.as_bytes(), "model").is_err());
    assert!(parse_anthropic_message_sse_bytes(anthropic.as_bytes(), "model").is_err());
    for ending in [
        "data: [DONE]",
        "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}",
    ] {
        assert!(
            parse_chat_completion_sse_bytes(format!("{chat}{ending}").as_bytes(), "model").is_ok()
        );
    }
    for ending in [
        "data: {\"type\":\"message_stop\"}",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}",
    ] {
        assert!(
            parse_anthropic_message_sse_bytes(format!("{anthropic}{ending}").as_bytes(), "model")
                .is_ok()
        );
    }
    assert!(
        parse_chat_completion_sse_bytes(
            format!("{chat}data: {{\"choices\":[{{\"index\":1,\"finish_reason\":\"stop\"}}]}}\n\n")
                .as_bytes(),
            "model"
        )
        .is_err()
    );
}

async fn mock_http_response(body: impl Into<String>) -> (String, tokio::task::JoinHandle<()>) {
    let body = body.into();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await.unwrap();
        socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    (format!("http://{address}/v1"), task)
}

#[tokio::test]
async fn websocket_terminal_state_resets_after_invalid_messages() {
    let (url, mock) = mock_http_response(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    )
    .await;
    let (config, _, model) = router_config(url);
    let router = LocalRouter::start(&config).await.unwrap();
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    for message in [
        "{broken",
        "null",
        "{}",
        "{\"type\":\"response.create\",\"stream\":false}",
    ] {
        socket
            .send(WebSocketMessage::Text(message.into()))
            .await
            .unwrap();
        assert_eq!(next_event(&mut socket).await["type"], "response.failed");
    }
    socket
        .send(WebSocketMessage::Text(
            json!({"type":"response.create","model":model,"input":"hello"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(next_event(&mut socket).await["type"], "response.completed");
    socket.close(None).await.unwrap();
    mock.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn adapted_sse_eof_preserves_completion_and_usage_across_transports() {
    for (protocol, content, stop) in [
        (
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1,\"total_tokens\":5}}",
        ),
        (
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-test\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":4}}}\n\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"hello\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}",
        ),
    ] {
        for transport in ["json", "sse", "websocket"] {
            for complete in [false, true] {
                let body = format!("{content}{}", if complete { stop } else { "" });
                let (url, mock) = mock_http_response(body).await;
                let (mut config, _, model) = router_config(url);
                config.profiles[0].upstream_protocol = protocol.into();
                config.profiles[0].normalize();
                let router = LocalRouter::start(&config).await.unwrap();
                let endpoint = router.endpoint();
                let response = if transport == "websocket" {
                    let mut socket = connect_router_websocket(&endpoint).await;
                    socket
                        .send(WebSocketMessage::Text(
                            json!({"type":"response.create","model":model,"input":"hello"})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                    let terminal = loop {
                        let event = next_event(&mut socket).await;
                        if responses_event_is_terminal(&event) {
                            break event;
                        }
                    };
                    assert_eq!(
                        terminal["type"],
                        if complete {
                            "response.completed"
                        } else {
                            "response.failed"
                        }
                    );
                    assert!(
                        tokio::time::timeout(Duration::from_millis(20), socket.next())
                            .await
                            .is_err()
                    );
                    socket.close(None).await.unwrap();
                    terminal["response"].clone()
                } else {
                    let reply = reqwest::Client::new()
                        .post(format!("{}/responses", endpoint.base_url))
                        .bearer_auth(&endpoint.token)
                        .json(&json!({"model":model,"input":"hello","stream":transport == "sse"}))
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(
                        reply.status().as_u16(),
                        if !complete && transport == "json" {
                            502
                        } else {
                            200
                        }
                    );
                    if transport == "json" {
                        reply.json::<Value>().await.unwrap()
                    } else {
                        let text = reply.text().await.unwrap();
                        let events = parse_responses_websocket_sse_events(&text).unwrap();
                        let terminals = events
                            .iter()
                            .filter(|event| responses_event_is_terminal(event))
                            .collect::<Vec<_>>();
                        assert_eq!(terminals.len(), 1);
                        assert_eq!(
                            terminals[0]["type"],
                            if complete {
                                "response.completed"
                            } else {
                                "response.failed"
                            }
                        );
                        terminals[0]["response"].clone()
                    }
                };
                if complete {
                    assert_eq!(response["status"], "completed", "{protocol} {transport}");
                    assert_eq!(response["usage"]["input_tokens"], 4);
                    assert_eq!(response["usage"]["output_tokens"], 1);
                }
                mock.await.unwrap();
                router.stop().await.unwrap();
            }
        }
    }
}

#[tokio::test]
async fn websocket_close_cancels_native_upstream_without_replay() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (started, ready) = oneshot::channel();
    let (closed, upstream_closed) = oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert!(matches!(
            socket.next().await,
            Some(Ok(WebSocketMessage::Text(_)))
        ));
        started.send(()).unwrap();
        assert!(matches!(
            socket.next().await,
            None | Some(Err(_)) | Some(Ok(WebSocketMessage::Close(_)))
        ));
        closed.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "cancelled request was replayed"
        );
    });
    let (mut config, _, model) = router_config(format!("http://{address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    socket
        .send(WebSocketMessage::Text(
            json!({"type":"response.create","model":model,"input":"hello"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    ready.await.unwrap();
    socket
        .send(WebSocketMessage::Ping(b"alive".to_vec().into()))
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(500), socket.next())
            .await
            .unwrap(),
        Some(Ok(WebSocketMessage::Pong(_)))
    ));
    let start = Instant::now();
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_millis(500), upstream_closed)
        .await
        .unwrap()
        .unwrap();
    println!(
        "CANCEL protocol=nativeWebSocket upstream_release_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(500), socket.next())
            .await
            .unwrap(),
        Some(Ok(WebSocketMessage::Close(_)))
    ));
    upstream.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn observed_websocket_close_is_logged_as_cancelled() {
    let (server, mut client) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(server);
    let probe = RouteRequestLogProbe::detached_test_probe();
    let _observer = probe.defer_finish().unwrap();
    client.close(None).await.unwrap();
    let mut observed = ObservedResponsesDownstream::new(&mut downstream, Some(probe.clone()));
    let error = tokio::time::timeout(
        Duration::from_millis(500),
        await_upstream(&mut observed, std::future::pending::<()>()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<DownstreamClosed>());
    let (status, code, _) = probe.projected_metadata_for_test();
    assert_eq!(status.as_deref(), Some("cancelled"));
    assert_eq!(code.as_deref(), Some("downstream_websocket_closed"));
}

#[tokio::test]
async fn streaming_terminal_is_attempted_once() {
    let (server, mut client) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(server);
    let bridge = ResponsesToolBridge::default();
    let mut state = ResponsesSseState::new("model", &bridge);
    state.finish(&mut downstream, None, None).await.unwrap();
    // Clear the transport guard so this assertion also exercises the state guard.
    downstream.clear_stream_id();
    state
        .fail(&mut downstream, "failure", "late failure")
        .await
        .unwrap();
    state.finish(&mut downstream, None, None).await.unwrap();
    let WebSocketMessage::Text(text) = client.next().await.unwrap().unwrap() else {
        panic!("expected completion");
    };
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["type"],
        "response.completed"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), client.next())
            .await
            .is_err()
    );
}

async fn next_event(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let WebSocketMessage::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test]
async fn adapted_websocket_failure_has_one_terminal_event() {
    for protocol in [
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        let (url, mock) = mock_http_response("data: {broken-json}\n\n").await;
        let (mut config, _, model) = router_config(url);
        config.profiles[0].upstream_protocol = protocol.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let mut socket = connect_router_websocket(&router.endpoint()).await;
        socket
            .send(WebSocketMessage::Text(
                json!({"type":"response.create","model":model,"input":"hello"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let event = next_event(&mut socket).await;
            if responses_event_is_terminal(&event) {
                assert_eq!(event["type"], "response.failed");
                break;
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), socket.next())
                .await
                .is_err(),
            "duplicate terminal event"
        );
        socket.close(None).await.unwrap();
        mock.await.unwrap();
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn websocket_close_cancels_http_headers_and_stream_reads_for_every_protocol() {
    for protocol in [
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        for phase in ["headers", "sniff", "stream", "error_body", "json_body"] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let (started, request_started) = oneshot::channel();
            let upstream = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_http_request(&mut socket).await.unwrap();
                if phase != "headers" {
                    let status = if phase == "error_body" {
                        "429 Too Many Requests"
                    } else {
                        "200 OK"
                    };
                    let content_type = if phase == "json_body" {
                        "application/json"
                    } else {
                        "text/event-stream"
                    };
                    socket.write_all(format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n\r\n").as_bytes()).await.unwrap();
                    if phase == "stream" {
                        let event = match protocol {
                            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS => {
                                json!({"choices":[{"index":0,"delta":{"content":"hello"}}]})
                            }
                            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES => {
                                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}})
                            }
                            _ => json!({"type":"response.output_text.delta","delta":"hello"}),
                        };
                        write_chunked_frame(
                            &mut socket,
                            format!("data: {event}\n\n").as_bytes(),
                            "test stream prefix",
                        )
                        .await
                        .unwrap();
                    } else if phase == "json_body" {
                        write_chunked_frame(&mut socket, b"{\"id\":", "test partial JSON")
                            .await
                            .unwrap();
                    }
                }
                started.send(()).unwrap();
                let mut buffer = [0; 1];
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => {}
                    other => panic!("unexpected upstream data: {other:?}"),
                }
            });
            let (mut config, _, model) = router_config(format!("http://{address}/v1"));
            config.profiles[0].upstream_protocol = protocol.into();
            config.profiles[0].normalize();
            let router = LocalRouter::start(&config).await.unwrap();
            let mut socket = connect_router_websocket(&router.endpoint()).await;
            socket
                .send(WebSocketMessage::Text(
                    json!({"type":"response.create","model":model,"input":"hello"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            request_started.await.unwrap();
            socket
                .send(WebSocketMessage::Ping(b"alive".to_vec().into()))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_millis(500), async {
                loop {
                    match socket.next().await.unwrap().unwrap() {
                        WebSocketMessage::Pong(payload) => {
                            assert_eq!(&payload[..], b"alive");
                            break;
                        }
                        WebSocketMessage::Text(text) => assert!(!responses_event_is_terminal(
                            &serde_json::from_str(&text).unwrap()
                        )),
                        other => panic!("unexpected message {other:?}"),
                    }
                }
            })
            .await
            .expect("Ping blocked behind the upstream");
            let start = Instant::now();
            socket.close(None).await.unwrap();
            tokio::time::timeout(Duration::from_millis(500), upstream)
                .await
                .expect("Close did not cancel upstream")
                .unwrap();
            println!(
                "CANCEL protocol={protocol} phase={phase} upstream_release_ms={:.3}",
                start.elapsed().as_secs_f64() * 1000.0
            );
            tokio::time::timeout(Duration::from_millis(500), async {
                while let Some(message) = socket.next().await {
                    match message.unwrap() {
                        WebSocketMessage::Close(_) => return,
                        WebSocketMessage::Text(text) => assert!(
                            !responses_event_is_terminal(&serde_json::from_str(&text).unwrap()),
                            "cancellation emitted terminal: {text}"
                        ),
                        other => panic!("unexpected message after Close: {other:?}"),
                    }
                }
            })
            .await
            .expect("missing close acknowledgement");
            router.stop().await.unwrap();
        }
    }
}

#[tokio::test]
async fn pending_websocket_requests_keep_order_and_release_budget() {
    let (server, mut client) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(server);
    let budget = downstream.request_body_budget.clone();
    let original_budget = budget.available_permits();
    let (release, wait) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        downstream.wait_for_upstream(wait).await.unwrap().unwrap();
        assert_eq!(downstream.pending_messages.len(), 2);
        assert!(budget.available_permits() < original_budget);
        for expected in ["first", "second"] {
            assert_eq!(
                downstream.next_message().await.unwrap(),
                Some(WebSocketMessage::Text(expected.into()))
            );
        }
        assert_eq!(budget.available_permits(), original_budget);
    });
    for message in ["first", "second"] {
        client
            .send(WebSocketMessage::Text(message.into()))
            .await
            .unwrap();
    }
    client
        .send(WebSocketMessage::Ping(b"queued".to_vec().into()))
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(500), client.next())
            .await
            .unwrap(),
        Some(Ok(WebSocketMessage::Pong(_)))
    ));
    release.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn http_write_half_shutdown_still_receives_response() {
    let (url, upstream) = mock_http_response(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
    )
    .await;
    let (config, _, model) = router_config(url);
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let url = reqwest::Url::parse(&endpoint.base_url).unwrap();
    let mut socket = TcpStream::connect(("127.0.0.1", url.port().unwrap()))
        .await
        .unwrap();
    let body = json!({"model":model,"input":"hello","stream":true}).to_string();
    socket.write_all(format!("POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}", endpoint.token, body.len()).as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8(response)
            .unwrap()
            .contains("response.completed")
    );
    upstream.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn pending_websocket_requests_are_bounded_by_count_and_shared_budget() {
    for permits in [0, REQUEST_BODY_BUDGET_PERMITS] {
        let (server, mut client) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(server);
        let budget = Arc::new(Semaphore::new(permits));
        downstream.request_body_budget = budget.clone();
        for _ in 0..9 {
            client
                .send(WebSocketMessage::Text("pending".into()))
                .await
                .unwrap();
        }
        let result = downstream
            .wait_for_upstream(tokio::time::timeout(
                Duration::from_millis(30),
                std::future::pending::<()>(),
            ))
            .await
            .unwrap();
        assert!(result.is_err());
        assert_eq!(
            downstream.pending_messages.len(),
            if permits == 0 { 1 } else { 8 }
        );
        assert_eq!(downstream.pending_budget_blocked, permits == 0);
        downstream.pending_messages.clear();
        assert_eq!(budget.available_permits(), permits);
    }
}

#[tokio::test]
async fn downstream_pings_do_not_restart_the_upstream_deadline() {
    let (server, mut client) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(server);
    let ping = tokio::spawn(async move {
        let mut count = 0;
        loop {
            client
                .send(WebSocketMessage::Ping(Default::default()))
                .await
                .unwrap();
            if client.next().await.is_none() {
                break;
            }
            count += 1;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        count
    });
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        downstream.wait_for_upstream(tokio::time::timeout(
            Duration::from_millis(40),
            std::future::pending::<()>(),
        )),
    )
    .await
    .expect("Ping restarted the deadline")
    .unwrap();
    assert!(result.is_err());
    ping.abort();
}
