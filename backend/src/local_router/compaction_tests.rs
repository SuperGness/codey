use super::tests::router_config;
use super::*;
use crate::config::RemoteCompactionProtocol;

fn compact_result() -> Value {
    json!({
        "id":"cmp_window", "object":"response.compaction", "created_at":123,
        "output":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"keep this task"}]},
            {"type":"compaction","id":"cmp_item","encrypted_content":"opaque-window"},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"retained answer"}]}
        ],
        "usage":{"input_tokens":1234,"output_tokens":45,"total_tokens":1279}
    })
}

#[test]
fn gateway_compaction_summary_round_trip_preserves_text_and_rejects_invalid_encoding() {
    use base64::Engine as _;
    let text = "用户目标：继续实现\n已验证：工具结果正常";
    let item = json!({"type":"compaction","encrypted_content":format!("{CODEY_SUMMARY_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text))});
    let mut body = json!({"model":"test-model","input":[item]});
    assert!(validate_cross_route_context(&body).is_ok());
    assert!(restore_codey_compaction_summaries(&mut body).unwrap());
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["text"], text);
    assert!(responses_to_chat_completions_body(&body).is_ok());
    assert!(responses_to_anthropic_messages_body(&body).is_ok());
    for content in [
        CODEY_SUMMARY_PREFIX.to_string(),
        format!("{CODEY_SUMMARY_PREFIX}%%%"),
        format!("{CODEY_SUMMARY_PREFIX}_w"),
    ] {
        assert!(
            codey_summary_message(&json!({"type":"compaction","encrypted_content":content}))
                .is_err()
        );
    }
    assert!(
        validate_cross_route_context(
            &json!({"input":[{"type":"compaction","encrypted_content":"native-opaque"}]})
        )
        .is_err()
    );
}

#[test]
fn compaction_adapter_preserves_history_and_all_compact_context_fields() {
    let expected = json!({
        "model":"upstream-model",
        "input":[
            {"role":"user","content":"original task"},
            {"type":"reasoning","id":"rs1","encrypted_content":"opaque-reasoning"},
            {"type":"compaction","encrypted_content":"previous-window"},
            {"type":"function_call","call_id":"call1","name":"tool","arguments":"{}"},
            {"type":"function_call_output","call_id":"call1","output":"tool result"}
        ],
        "instructions":"preserve these instructions",
        "previous_response_id":"resp_previous",
        "prompt_cache_key":"cache-key", "prompt_cache_options":{"retention":"in_memory"},
        "prompt_cache_retention":"24h", "service_tier":"priority"
    });
    let mut body = expected.clone();
    body["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"compaction_trigger"}));
    body["stream"] = json!(true);
    body["tools"] = json!([{"type":"web_search"}]);
    body["reasoning"] = json!({"effort":"high"});
    body["store"] = json!(false);
    body["include"] = json!(["reasoning.encrypted_content"]);
    body["text"] = json!({"format":{"type":"text"}});
    body["parallel_tool_calls"] = json!(true);
    body["max_output_tokens"] = json!(100);
    adapt_compaction_trigger_to_compact(&mut body).unwrap();
    assert_eq!(body, expected);
}

fn unsupported_compaction_bodies() -> Vec<Value> {
    vec![
        json!({"input":[{"type":"compaction_trigger"},{"role":"user","content":"later task"}]}),
        json!({"input":[{"type":"compaction_trigger"},{"type":"compaction_trigger"}]}),
        json!({"input":[{"type":"compaction_trigger","unknown_option":true}]}),
        json!({"input":{"type":"compaction_trigger"}}),
        json!({"input":[{"type":"compaction_trigger"}],"conversation":"conv_remote"}),
        json!({"input":[{"type":"compaction_trigger"}],"prompt":{"id":"prompt_remote"}}),
    ]
}

#[test]
fn compaction_adapter_rejects_unknown_semantics_without_modifying_input() {
    for original in unsupported_compaction_bodies() {
        let mut body = original.clone();
        assert!(
            adapt_compaction_trigger_to_compact(&mut body).is_err(),
            "{original}"
        );
        assert_eq!(body, original);
    }
}

#[tokio::test]
async fn compaction_compatibility_hot_reload_preserves_http_and_sse_results() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider, model) =
        router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    config.profiles[0].supports_remote_compaction = true;
    let router = LocalRouter::start(&config).await.unwrap();
    // 接口选择热更新，不改变已启动的 Provider 能力或地址。
    config.profiles[0].remote_compaction_protocol = RemoteCompactionProtocol::CompactEndpoint;
    router.update_config(&config);
    let endpoint = router.endpoint();
    assert!(endpoint.supports_remote_compaction);
    let upstream_task = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            assert_eq!(request.path, "/v1/responses/compact");
            assert_eq!(
                incoming_header(&request, "authorization"),
                Some("Bearer sk-upstream")
            );
            assert_eq!(
                incoming_header(&request, "accept"),
                Some("application/json")
            );
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(
                body,
                json!({
                    "model":"provider-model", "instructions":"keep instructions",
                    "previous_response_id":"resp_previous",
                    "input":[
                        {"role":"user","content":"keep this task"},
                        {"type":"compaction","encrypted_content":"old-window"}
                    ]
                })
            );
            write_json_response(&mut socket, 200, &compact_result())
                .await
                .unwrap();
        }
        // 普通生成仍然走 Responses，不能被线路的压缩接口设置改写。
        let (mut socket, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut socket).await.unwrap();
        assert_eq!(request.path, "/v1/responses");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["input"], "ordinary request");
        write_json_response(
            &mut socket,
            200,
            &json!({"id":"resp_normal","status":"completed","output":[]}),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for stream in [false, true] {
        let response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("accept", "text/event-stream")
            .json(&json!({
                "model":model_alias(&provider, &model), "stream":stream,
                "instructions":"keep instructions", "previous_response_id":"resp_previous",
                "tools":[{"type":"web_search"}], "store":false,
                "input":[
                    {"role":"user","content":"keep this task"},
                    {"type":"compaction","encrypted_content":"old-window"},
                    {"type":"compaction_trigger"}
                ]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let result = if stream {
            let events =
                parse_responses_websocket_sse_events(&response.text().await.unwrap()).unwrap();
            assert_eq!(events.last().unwrap()["type"], "response.completed");
            let items: Vec<_> = events
                .iter()
                .filter(|event| event["type"] == "response.output_item.done")
                .map(|event| event["item"].clone())
                .collect();
            assert_eq!(items, *compact_result()["output"].as_array().unwrap());
            events.last().unwrap()["response"].clone()
        } else {
            response.json::<Value>().await.unwrap()
        };
        let mut expected = compact_result();
        expected["object"] = json!("response");
        expected["status"] = json!("completed");
        assert_eq!(result, expected);
    }
    let response = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":model_alias(&provider, &model), "input":"ordinary request"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn compaction_compatibility_rejects_invalid_payloads_before_upstream() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider, model) =
        router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    config.profiles[0].supports_remote_compaction = true;
    config.profiles[0].remote_compaction_protocol = RemoteCompactionProtocol::CompactEndpoint;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = reqwest::Client::new();
    for mut body in unsupported_compaction_bodies() {
        body["model"] = json!(model_alias(&provider, &model));
        body["stream"] = json!(true);
        let response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "unsupported_compaction_payload"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), upstream.accept())
            .await
            .is_err()
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn compaction_compatibility_rejects_bad_results_and_does_not_retry() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider, model) =
        router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    config.profiles[0].supports_remote_compaction = true;
    config.profiles[0].remote_compaction_protocol = RemoteCompactionProtocol::CompactEndpoint;
    let cases = [
        (
            200,
            json!({"output":[]}),
            502,
            "invalid_compaction_response",
        ),
        (
            200,
            json!({"output":[{"type":"compaction","encrypted_content":""}]}),
            502,
            "invalid_compaction_response",
        ),
        (
            200,
            json!({"status":"failed","output":[{"type":"compaction","encrypted_content":"bad"}]}),
            502,
            "invalid_compaction_response",
        ),
        (
            200,
            json!({"status":"incomplete","output":[{"type":"compaction","encrypted_content":"partial"}]}),
            502,
            "invalid_compaction_response",
        ),
        (
            200,
            json!({"error":{"code":"server_error"},"output":[{"type":"compaction","encrypted_content":"bad"}]}),
            502,
            "invalid_compaction_response",
        ),
        (
            200,
            json!({"error":{"code":"context_length_exceeded","message":"context full"}}),
            400,
            "context_length_exceeded",
        ),
        (
            429,
            json!({"error":{"code":"rate_limit_exceeded","message":"rate limited"}}),
            429,
            "rate_limit_exceeded",
        ),
    ];
    let upstream_cases = cases.clone();
    let upstream_task = tokio::spawn(async move {
        for (status, body, _, _) in upstream_cases {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            assert_eq!(request.path, "/v1/responses/compact");
            write_json_response(&mut socket, status, &body)
                .await
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), upstream.accept())
                .await
                .is_err()
        );
    });
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for (_, _, expected_status, expected_code) in cases {
        let response = client.post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token).header("thread-id", "same-compaction-task")
            .json(&json!({"model":model_alias(&provider, &model),"stream":true,"input":[{"type":"compaction_trigger"}]}))
            .send().await.unwrap();
        assert_eq!(response.status().as_u16(), expected_status);
        if expected_status == 429 {
            assert!(
                response.headers()[CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with("text/plain")
            );
            let body = response.text().await.unwrap();
            assert!(body.contains(expected_code), "{body}");
        } else {
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"]["code"],
                expected_code
            );
        }
    }
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn gateway_compaction_logs_the_resolved_route_and_model() {
    let logs = tempfile::tempdir().unwrap();
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider, model) =
        router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    let mock = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut socket).await.unwrap();
        assert_eq!(request.path, "/v1/responses");
        write_json_response(&mut socket, 200, &json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"交接摘要"}]}]})).await.unwrap();
    });
    config.route_request_log.enabled = true;
    config.route_request_log.backend = crate::config::RouteRequestLogBackend::Sqlite;
    let router = LocalRouter::start_with_logger(
        &config,
        Arc::new(RouteRequestLogController::with_root(
            logs.path().to_path_buf(),
        )),
    )
    .await
    .unwrap();
    let endpoint = router.endpoint();
    let response = reqwest::Client::new().post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":model_alias(&provider, &model),"input":[{"type":"compaction_trigger"}]}))
        .send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let value = response.json::<Value>().await.unwrap();
    assert!(
        value["output"][0]["encrypted_content"]
            .as_str()
            .unwrap()
            .starts_with(CODEY_SUMMARY_PREFIX)
    );
    mock.await.unwrap();
    router.stop().await.unwrap();
    let page = crate::route_request_log::query_route_request_logs(
        logs.path(),
        crate::config::RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].provider.as_deref(), Some(provider.as_str()));
    assert_eq!(page.items[0].provider_name.as_deref(), Some("Relay"));
    assert_eq!(page.items[0].model.as_deref(), Some(model.as_str()));
    assert_eq!(page.items[0].error_code.as_deref(), None);
}

#[tokio::test]
async fn gateway_compaction_returns_portable_summary_on_all_protocols() {
    let protocols = [
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
        crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
    ];
    for (index, protocol) in protocols.iter().enumerate() {
        let protocol = *protocol;
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider, model) =
            router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
        config.profiles[0].upstream_protocol = protocol.into();
        config.profiles[0].supports_remote_compaction = false;
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let next = protocols[(index + 1) % protocols.len()];
        let mock = tokio::spawn(async move {
            for protocol in [protocol, next] {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert!(!body.to_string().contains("compaction_trigger"));
                assert!(!body.to_string().contains(CODEY_SUMMARY_PREFIX));
                if protocol == crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS {
                    assert_eq!(request.path, "/v1/chat/completions");
                    write_json_response(&mut socket, 200, &json!({"id":"chat_summary","choices":[{"index":0,"message":{"role":"assistant","content":"交接摘要：继续实现"},"finish_reason":"stop"}]})).await.unwrap();
                } else if protocol == crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES {
                    assert_eq!(request.path, "/v1/messages");
                    write_json_response(&mut socket, 200, &json!({"id":"msg_summary","type":"message","role":"assistant","content":[{"type":"text","text":"交接摘要：继续实现"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}})).await.unwrap();
                } else {
                    assert_eq!(request.path, "/v1/responses");
                    write_json_response(&mut socket, 200, &json!({"id":"resp_summary","object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"交接摘要：继续实现"}]}]})).await.unwrap();
                }
                assert!(body.get("tools").is_none());
            }
        });
        let client = reqwest::Client::new();
        let response = client.post(format!("{}/responses", endpoint.base_url)).bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider, &model),"stream":index == 0,"input":[{"role":"user","content":"继续实现"},{"type":"compaction_trigger"}],"tools":[{"type":"function","name":"unused","parameters":{"type":"object"}}],"tool_choice":"auto","parallel_tool_calls":true})).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let value = if index == 0 {
            let events = response.text().await.unwrap();
            let frames: Vec<Value> = events
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .filter_map(|data| serde_json::from_str(data).ok())
                .collect();
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| frame["type"] == "response.output_item.done"
                        && frame["item"]["type"] == "compaction")
                    .count(),
                1
            );
            frames
                .into_iter()
                .find(|frame| frame["type"] == "response.completed")
                .unwrap()["response"]
                .clone()
        } else {
            response.json::<Value>().await.unwrap()
        };
        validate_compaction_result(&value, true).unwrap();
        let item = value["output"][0].clone();
        assert!(
            item["encrypted_content"]
                .as_str()
                .unwrap()
                .starts_with(CODEY_SUMMARY_PREFIX)
        );
        config.profiles[0].upstream_protocol = next.into();
        config.profiles[0].normalize();
        router.update_config(&config);
        let response = client.post(format!("{}/responses", endpoint.base_url)).bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider, &model),"input":[item,{"role":"user","content":"继续"}]})).send().await.unwrap();
        assert_eq!(
            response.status().as_u16(),
            200,
            "{}",
            response.text().await.unwrap()
        );
        mock.await.unwrap();
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn gateway_compaction_collects_native_sse_and_rejects_empty_or_failed_summaries() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (config, provider, model) =
        router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let mock = tokio::spawn(async move {
        for index in 0..4 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            assert_eq!(request.path, "/v1/responses");
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["stream"], true);
            assert_eq!(body["input"][0]["role"], "user");
            match index {
                0 => {
                    let sse = concat!(
                        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"摘要\"}]}}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"
                    );
                    socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}", sse.len()).as_bytes()).await.unwrap();
                }
                1 => write_json_response(
                    &mut socket,
                    200,
                    &json!({"status":"completed","output":[]}),
                )
                .await
                .unwrap(),
                2 => write_json_response(
                    &mut socket,
                    400,
                    &json!({"error":{"code":"context_length_exceeded"}}),
                )
                .await
                .unwrap(),
                _ => write_json_response(&mut socket, 500, &json!({"error":{"message":"failed"}}))
                    .await
                    .unwrap(),
            }
        }
    });
    for (status, code) in [
        (200, ""),
        (502, "invalid_compaction_response"),
        (400, CONTEXT_LENGTH_EXCEEDED),
        (502, "invalid_compaction_response"),
    ] {
        let response = reqwest::Client::new()
            .post(format!("{}/responses/compact", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider, &model),"input":"继续任务"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        let body = response.json::<Value>().await.unwrap();
        if status == 200 {
            validate_compaction_result(&body, true).unwrap();
        } else {
            assert_eq!(body["error"]["code"], code);
        }
    }
    mock.await.unwrap();
    router.stop().await.unwrap();
}
