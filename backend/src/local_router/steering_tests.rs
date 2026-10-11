use super::*;

async fn receive(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let WebSocketMessage::Text(text) = message else {
        panic!("expected JSON event")
    };
    serde_json::from_str(text.as_str()).unwrap()
}

fn response_event(sequence: usize, completed: bool, tool: bool) -> Value {
    let output = if tool && sequence == 1 && completed {
        json!([{"type":"function_call","call_id":"call_one","name":"lookup","arguments":"{}"}])
    } else {
        json!([])
    };
    json!({"type":if completed {"response.completed"} else {"response.created"}, "sequence_number":if completed {1} else {0},
        "response":{"id":format!("resp-steer-{sequence}"),"object":"response",
        "status":if completed {"completed"} else {"in_progress"},"output":output}})
}

async fn run_steering_roundtrip(websocket: bool, tool: bool, late: bool) {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        let mut release_rx = Some(release_rx);
        if websocket {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for sequence in 1..=2 {
                let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected create")
                };
                requests.push(serde_json::from_str::<Value>(&text).unwrap());
                socket
                    .send(WebSocketMessage::Text(
                        response_event(sequence, false, tool).to_string().into(),
                    ))
                    .await
                    .unwrap();
                if let Some(release) = release_rx.take() {
                    release.await.unwrap();
                }
                socket
                    .send(WebSocketMessage::Text(
                        response_event(sequence, true, tool).to_string().into(),
                    ))
                    .await
                    .unwrap();
            }
            // 保持连接到客户端收到终态，避免制造与本测试无关的断线。
            let _ = socket.next().await;
        } else {
            for sequence in 1..=2 {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                requests.push(serde_json::from_slice::<Value>(&request.body).unwrap());
                stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
                write_test_http_chunk(
                    &mut stream,
                    &format!("data: {}\n\n", response_event(sequence, false, tool)),
                )
                .await;
                if let Some(release) = release_rx.take() {
                    release.await.unwrap();
                }
                write_test_http_chunk(
                    &mut stream,
                    &format!("data: {}\n\n", response_event(sequence, true, tool)),
                )
                .await;
                stream.write_all(b"0\r\n\r\n").await.unwrap();
            }
        }
        requests
    });
    let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
    config.profiles[0].supports_websockets = websocket;
    let router = LocalRouter::start(&config).await.unwrap();
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    let alias = model_alias(&provider, &model);
    socket.send(WebSocketMessage::Text(json!({"type":"response.create","model":alias,
        "input":"original task","instructions":"original settings","stream_id":"lane-one",
        "tools":[{"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}]
    }).to_string().into())).await.unwrap();
    assert_eq!(receive(&mut socket).await["type"], "response.created");
    let mut release_tx = Some(release_tx);
    if late {
        release_tx.take().unwrap().send(()).unwrap();
        assert_eq!(receive(&mut socket).await["type"], "response.completed");
    }
    socket.send(WebSocketMessage::Text(json!({"type":"response.steer","previous_response_id":"resp-steer-1", "input":"new requirement"}).to_string().into())).await.unwrap();
    let accepted = receive(&mut socket).await;
    assert_eq!(accepted["type"], "response.steer.accepted");
    assert_eq!(accepted["stream_id"], "lane-one");
    if !late {
        socket.send(WebSocketMessage::Text(json!({"type":"response.steer","previous_response_id":"resp-steer-1", "input":"second requirement"}).to_string().into())).await.unwrap();
        let second = receive(&mut socket).await;
        assert_eq!(second["type"], "response.steer.accepted");
        assert!(second["sequence_number"].as_u64() > accepted["sequence_number"].as_u64());
        release_tx.take().unwrap().send(()).unwrap();
        let completed = receive(&mut socket).await;
        assert_eq!(completed["type"], "response.completed");
        assert!(completed["sequence_number"].as_u64() > second["sequence_number"].as_u64());
    }
    if tool {
        let pending = receive(&mut socket).await;
        assert_eq!(pending["type"], "response.steer.pending");
        assert_eq!(pending["steer"]["id"], accepted["steer"]["id"]);
        assert_eq!(pending["required_input"][0]["call_id"], "call_one");
        if !late {
            assert_eq!(receive(&mut socket).await["type"], "response.steer.pending");
        }
        // 无效重试不得覆盖旧响应、切换待处理消息的 lane 或消费已接受的输入。
        for input in [
            json!([]),
            json!([
                {"type":"function_call_output","call_id":"call_one","output":"x"},
                {"type":"function_call_output","call_id":"call_one","output":"x"}
            ]),
            json!([{"type":"function_call_output","call_id":"call_one"}]),
        ] {
            socket
                .send(WebSocketMessage::Text(
                    json!({"type":"response.create","model":alias,
                        "previous_response_id":"resp-steer-1","stream_id":"bad-lane","input":input
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let error = receive(&mut socket).await;
            assert_eq!(error["type"], "response.failed");
            assert_eq!(error["stream_id"], "bad-lane");
        }
        socket.send(WebSocketMessage::Text(json!({"type":"response.create","model":alias,
            "previous_response_id":"resp-steer-1","instructions":"explicit settings",
            "stream_id":"lane-one","input":[{"type":"function_call_output","call_id":"call_one","output":"tool result"}]
        }).to_string().into())).await.unwrap();
    }
    let created = receive(&mut socket).await;
    assert_eq!(created["type"], "response.created", "{created}");
    assert_eq!(created["response"]["id"], "resp-steer-2");
    assert_eq!(created["stream_id"], "lane-one");
    assert_eq!(receive(&mut socket).await["type"], "response.completed");
    socket.close(None).await.unwrap();
    router.stop().await.unwrap();
    let requests = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]["instructions"],
        if tool {
            "explicit settings"
        } else {
            "original settings"
        }
    );
    assert_eq!(requests[1]["model"], model);
    let input = requests[1]["input"].as_array().unwrap();
    let updates: Vec<_> = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item.to_string().contains("new requirement"))
        .collect();
    assert_eq!(
        updates.len(),
        1,
        "steering must be applied exactly once: {input:?}"
    );
    if !late {
        let second: Vec<_> = input
            .iter()
            .enumerate()
            .filter(|(_, item)| item.to_string().contains("second requirement"))
            .collect();
        assert_eq!(second.len(), 1);
        assert!(updates[0].0 < second[0].0);
    }
    if tool {
        let output = input
            .iter()
            .position(|item| item["type"] == "function_call_output")
            .unwrap();
        assert!(
            updates[0].0 < output,
            "queued steering precedes explicit input"
        );
    }
}

#[tokio::test]
async fn websocket_steering_continues_with_original_settings() {
    run_steering_roundtrip(true, false, false).await;
}

#[tokio::test]
async fn http_fallback_steering_continues_with_original_settings() {
    run_steering_roundtrip(false, false, false).await;
}

#[tokio::test]
async fn websocket_steering_waits_for_tool_results() {
    run_steering_roundtrip(true, true, false).await;
}

#[tokio::test]
async fn http_fallback_steering_waits_for_tool_results() {
    run_steering_roundtrip(false, true, false).await;
}

#[tokio::test]
async fn websocket_late_steering_continues_after_completion() {
    run_steering_roundtrip(true, false, true).await;
}

#[tokio::test]
async fn http_fallback_late_steering_continues_after_completion() {
    run_steering_roundtrip(false, false, true).await;
}

#[tokio::test]
async fn invalid_steering_is_a_control_failure_and_keeps_socket_usable() {
    let (config, _, _) = router_config("http://127.0.0.1:9/v1".into());
    let router = LocalRouter::start(&config).await.unwrap();
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    for (request, code) in [
        (
            json!({"type":"response.steer","input":"keep this"}),
            "invalid_input",
        ),
        (
            json!({"type":"response.steer","previous_response_id":"another-connection","input":"keep this"}),
            "response_not_found",
        ),
    ] {
        socket
            .send(WebSocketMessage::Text(request.to_string().into()))
            .await
            .unwrap();
        let event = receive(&mut socket).await;
        assert_eq!(event["type"], "response.steer.failed");
        assert_eq!(event["error"]["code"], code);
        assert_eq!(event["steer"]["input"], "keep this");
    }
    socket.close(None).await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn websocket_interrupt_is_forwarded_to_upstream() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let WebSocketMessage::Text(create) = socket.next().await.unwrap().unwrap() else {
            panic!("expected response.create")
        };
        let create: Value = serde_json::from_str(&create).unwrap();
        assert_eq!(create["type"], "response.create");
        socket
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.created",
                    "response":{"id":"resp-interrupt","object":"response","status":"in_progress","output":[]}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let WebSocketMessage::Text(interrupt) = socket.next().await.unwrap().unwrap() else {
            panic!("expected response.interrupt")
        };
        let interrupt: Value = serde_json::from_str(&interrupt).unwrap();
        assert_eq!(interrupt["type"], "response.interrupt");
        assert_eq!(interrupt["response_id"], "resp-interrupt");
        assert_eq!(interrupt["mode"], "discard_partial_items");
        socket
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.incomplete",
                    "response":{"id":"resp-interrupt","object":"response","status":"incomplete","output":[]}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let _ = socket.next().await;
    });

    let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    socket
        .send(WebSocketMessage::Text(
            json!({
                "type":"response.create",
                "model":model_alias(&provider, &model),
                "input":"interrupt me",
                "stream_id":"interrupt-lane"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    assert_eq!(receive(&mut socket).await["type"], "response.created");
    socket
        .send(WebSocketMessage::Text(
            json!({
                "type":"response.interrupt",
                "response_id":"resp-interrupt",
                "mode":"discard_partial_items"
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    assert_eq!(receive(&mut socket).await["type"], "response.incomplete");
    socket.close(None).await.unwrap();
    router.stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
