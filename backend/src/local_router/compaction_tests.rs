use super::tests::router_config;
use super::*;
use crate::config::RemoteCompactionProtocol;

#[test]
fn gateway_summary_budget_uses_route_alias_without_cross_route_fallback() {
    let metadata = BTreeMap::from([
        (
            "model".into(),
            json!({"context_window":1000000})
                .as_object()
                .unwrap()
                .clone(),
        ),
        (
            "route-a/model".into(),
            json!({"context_window":32768}).as_object().unwrap().clone(),
        ),
        (
            "route-b/model".into(),
            json!({"context_window":8192}).as_object().unwrap().clone(),
        ),
    ]);
    for provider in ["route-a", " ROUTE-A "] {
        let selected = gateway_compaction_model_metadata(&metadata, provider, "model").unwrap();
        assert_eq!(selected["context_window"], 32768);
    }
    assert!(gateway_compaction_model_metadata(&metadata, "route-c", "model").is_none());
}

#[test]
fn gateway_summary_budget_respects_window_threshold_and_explicit_limits() {
    let metadata = json!({"context_window": 32768, "effective_context_window_percent": 75, "auto_compact_token_limit": 24000});
    assert_eq!(
        gateway_compaction_output_budget(&json!({}), metadata.as_object()).unwrap(),
        3072
    );
    let threshold = json!({"auto_compact_token_limit": 4096});
    assert_eq!(
        gateway_compaction_output_budget(&json!({}), threshold.as_object()).unwrap(),
        1024
    );
    assert_eq!(
        gateway_compaction_output_budget(&json!({"max_output_tokens":64000}), None).unwrap(),
        16384
    );
    assert_eq!(
        gateway_compaction_output_budget(&json!({"max_output_tokens":null}), None).unwrap(),
        16384
    );
    assert_eq!(
        gateway_compaction_output_budget(&json!({"max_output_tokens":1}), None).unwrap(),
        1
    );
    for invalid in [json!(0), json!(-1), json!("100")] {
        assert!(
            gateway_compaction_output_budget(&json!({"max_output_tokens":invalid}), None).is_err()
        );
    }
    let tiny = json!({"auto_compact_token_limit": 1});
    assert!(gateway_compaction_output_budget(&json!({}), tiny.as_object()).is_err());
}

#[test]
fn gateway_summary_budget_preserves_instructions_and_history() {
    let history = json!([{"role":"system","content":"retain system constraints"}, {"role":"user","content":"task"}]);
    let mut body =
        json!({"instructions":"retain instructions", "input":history, "max_tokens":64000});
    body["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"compaction_trigger"}));
    prepare_gateway_compaction(&mut body, 4096).unwrap();
    assert_eq!(body["instructions"], "retain instructions");
    assert_eq!(
        &body["input"].as_array().unwrap()[..2],
        history.as_array().unwrap()
    );
    assert_eq!(body["max_output_tokens"], 4096);
    assert!(body.get("max_tokens").is_none());
    assert!(
        body["input"][2]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("2048 token")
    );
}

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
    assert!(!restore_codey_compaction_summaries(&mut body).unwrap());
    assert_eq!(body["input"].as_array().unwrap().len(), 1);
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
        // 压缩前输入的使用量不能成为新摘要窗口的 token 计数。
        assert!(value.get("usage").is_none());
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
async fn gateway_compaction_rejects_truncated_summaries_on_all_protocols() {
    for (protocol, truncated) in [
        (
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            json!({"id":"chat","choices":[{"index":0,"message":{"role":"assistant","content":"partial summary"},"finish_reason":"length"}]}),
        ),
        (
            crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
            json!({"id":"msg","type":"message","role":"assistant","content":[{"type":"text","text":"partial summary"}],"stop_reason":"max_tokens","usage":{"input_tokens":100000,"output_tokens":1}}),
        ),
        (
            crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
            json!({"id":"resp","status":"incomplete","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"partial summary"}]}]}),
        ),
    ] {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider, model) =
            router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
        config.profiles[0].upstream_protocol = protocol.into();
        config.profiles[0].supports_remote_compaction = false;
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let mock = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut socket).await.unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let limit = body
                .get("max_output_tokens")
                .or_else(|| body.get("max_tokens"))
                .or_else(|| body.get("max_completion_tokens"))
                .unwrap();
            assert_eq!(limit, 128);
            write_json_response(&mut socket, 200, &truncated)
                .await
                .unwrap();
        });
        let response = reqwest::Client::new().post(format!("{}/responses", endpoint.base_url)).bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider, &model),"max_output_tokens":128,"input":[{"role":"user","content":"preserve this"},{"type":"compaction_trigger"}]})).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 502, "{protocol}");
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "invalid_compaction_response"
        );
        mock.await.unwrap();
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn gateway_compaction_http_errors_preserve_status_retry_and_diagnostics() {
    for protocol in [
        crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        for (status, code) in [
            (401, "invalid_api_key"),
            (403, "permission_denied"),
            (429, "insufficient_quota"),
            (429, "rate_limit_exceeded"),
            (503, "server_error"),
            (413, "context_length_exceeded"),
        ] {
            let (url, task, requests) = super::retry_contract_tests::error_upstream(
                status,
                json!({"error":{"code":code,"message":"summary failure sk-upstream"}}).to_string(),
                "application/json",
            )
            .await;
            let (mut config, provider, model) = router_config(url);
            config.profiles[0].upstream_protocol = protocol.into();
            config.profiles[0].supports_remote_compaction = false;
            config.profiles[0].normalize();
            let router = LocalRouter::start(&config).await.unwrap();
            let endpoint = router.endpoint();
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap();
            for (path, stream, input) in [
                ("responses/compact", false, json!("continue")),
                (
                    "responses",
                    true,
                    json!([{"role":"user","content":"continue"},{"type":"compaction_trigger"}]),
                ),
            ] {
                let response = client
                    .post(format!("{}/{path}", endpoint.base_url))
                    .bearer_auth(&endpoint.token)
                    .json(&json!({"model":model_alias(&provider,&model),"input":input,"stream":stream}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status().as_u16(), status, "{protocol}: {code}");
                assert_eq!(response.headers()["retry-after"], "1");
                assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
                let body = response.text().await.unwrap();
                for expected in [
                    "远程压缩摘要失败",
                    "Relay",
                    "provider-model",
                    "summary failure ***",
                    "retry-contract",
                    code,
                ] {
                    assert!(body.contains(expected), "{protocol}: {body}");
                }
                assert!(!body.contains("sk-upstream"), "{body}");
                assert!(!body.contains("invalid_compaction_response"), "{body}");
                let value: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(
                    value["error"]["code"],
                    if code == CONTEXT_LENGTH_EXCEEDED {
                        CONTEXT_LENGTH_EXCEEDED
                    } else {
                        "upstream_http_error"
                    }
                );
                assert_eq!(value["error"]["codey"]["httpStatus"], status);
                assert_eq!(value["error"]["codey"]["upstreamCode"], code);
                assert_eq!(
                    value["error"]["codey"]["upstreamRequestId"],
                    "retry-contract"
                );
                assert_eq!(value["error"]["codey"]["routeId"], provider);
                assert_eq!(
                    value["error"]["codey"]["compactionPhase"],
                    "gateway_summary"
                );
            }
            assert_eq!(requests.lock().unwrap().len(), 2, "{protocol}: {code}");
            router.stop().await.unwrap();
            task.abort();
        }
    }
}

#[tokio::test]
async fn gateway_compaction_websocket_failure_keeps_metadata_and_redacted_log() {
    let logs = tempfile::tempdir().unwrap();
    let (url, task, requests) = super::retry_contract_tests::error_upstream(
        429,
        json!({"error":{"code":"rate_limit_exceeded","type":"rate_limit_error","message":"summary failure sk-upstream"}}).to_string(),
        "application/json",
    ).await;
    let (mut config, provider, model) = router_config(url);
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
    let mut websocket = super::tests::connect_router_websocket(&router.endpoint()).await;
    websocket
        .send(WebSocketMessage::Text(
            json!({
                "type":"response.create", "model":model_alias(&provider, &model),
                "input":[{"role":"user","content":"continue"},{"type":"compaction_trigger"}]
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(message) = websocket.next().await {
            if let WebSocketMessage::Text(text) = message.unwrap() {
                let event: Value = serde_json::from_str(text.as_ref()).unwrap();
                if responses_event_is_terminal(&event) {
                    return event;
                }
            }
        }
        panic!("missing compaction failure event")
    })
    .await
    .unwrap();
    let error = &event["response"]["error"];
    assert_eq!(error["code"], "rate_limit_exceeded");
    assert_eq!(error["codey"]["compactionPhase"], "gateway_summary");
    assert_eq!(error["codey"]["httpStatus"], 429);
    assert_eq!(error["codey"]["routeId"], provider);
    assert_eq!(error["codey"]["upstreamCode"], "rate_limit_exceeded");
    assert_eq!(error["codey"]["upstreamType"], "rate_limit_error");
    assert_eq!(error["codey"]["upstreamRequestId"], "retry-contract");
    assert!(error["codey"]["retryAfterMs"].as_u64().unwrap() <= 1000);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("远程压缩摘要失败")
    );
    assert!(!event.to_string().contains("sk-upstream"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    websocket.close(None).await.unwrap();
    router.stop().await.unwrap();
    task.abort();
    let page = crate::route_request_log::query_route_request_logs(
        logs.path(),
        crate::config::RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.total, 1);
    let entry = &page.items[0];
    assert_eq!(entry.upstream_status_code, Some(429));
    assert_eq!(entry.upstream_request_id.as_deref(), Some("retry-contract"));
    let summary = entry.upstream_error_summary.as_deref().unwrap();
    assert!(summary.contains("summary failure ***"));
    assert!(!summary.contains("sk-upstream"));
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
        (500, "upstream_http_error"),
    ] {
        let response = reqwest::Client::new()
            .post(format!("{}/responses/compact", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider, &model),"input":"继续任务"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
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

#[tokio::test]
async fn gateway_compaction_http_200_failures_keep_upstream_diagnostics() {
    for protocol in [
        crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        for (code, status) in [
            ("rate_limit_exceeded", 429),
            ("insufficient_quota", 429),
            ("invalid_api_key", 401),
        ] {
            for sse in [false, true] {
                let value = json!({"type":"error","error":{"code":code,"message":"embedded failure sk-upstream"}});
                let (body, content_type) = if sse {
                    (format!("data: {value}\n\n"), "text/event-stream")
                } else {
                    (value.to_string(), "application/json")
                };
                let (url, task, requests) =
                    super::retry_contract_tests::error_upstream(200, body, content_type).await;
                let (mut config, provider, model) = router_config(url);
                config.profiles[0].upstream_protocol = protocol.into();
                config.profiles[0].normalize();
                let router = LocalRouter::start(&config).await.unwrap();
                let endpoint = router.endpoint();
                let response = reqwest::Client::new()
                    .post(format!("{}/responses/compact", endpoint.base_url))
                    .bearer_auth(&endpoint.token)
                    .json(&json!({"model":model_alias(&provider,&model),"input":"continue"}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    response.status().as_u16(),
                    status,
                    "{protocol}: {code}, sse={sse}"
                );
                assert_eq!(response.headers()["retry-after"], "1");
                assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
                let body = response.text().await.unwrap();
                for expected in [
                    code,
                    "embedded failure ***",
                    "远程压缩摘要失败",
                    "Relay",
                    "retry-contract",
                ] {
                    assert!(body.contains(expected), "{protocol}: {body}");
                }
                assert!(!body.contains("sk-upstream"), "{body}");
                let value: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(value["error"]["code"], "upstream_http_error");
                assert_eq!(value["error"]["codey"]["httpStatus"], status);
                assert_eq!(value["error"]["codey"]["upstreamCode"], code);
                assert_eq!(
                    value["error"]["codey"]["upstreamRequestId"],
                    "retry-contract"
                );
                assert_eq!(value["error"]["codey"]["routeId"], provider);
                assert_eq!(
                    value["error"]["codey"]["compactionPhase"],
                    "gateway_summary"
                );
                assert_eq!(requests.lock().unwrap().len(), 1);
                router.stop().await.unwrap();
                task.abort();
            }
        }
    }
}

#[test]
fn gateway_compaction_failures_preserve_handoff_phase_and_unavailable_details() {
    let (config, provider, _) = router_config("http://127.0.0.1:9/v1".into());
    let snapshot = RouterSnapshot::from_config(&config);
    let route = &snapshot.routes[&provider];
    for (phase, label, message) in [
        (
            GatewayCompactionPhase::GatewaySummary,
            "gateway_summary",
            "远程压缩摘要失败",
        ),
        (
            GatewayCompactionPhase::SourceRouteHandoff,
            "source_route_handoff",
            "源账号交接摘要失败",
        ),
        (
            GatewayCompactionPhase::TargetRouteHandoff,
            "target_route_handoff",
            "目标账号交接摘要失败",
        ),
    ] {
        for (error, status, code) in [
            (ContextLengthExceeded.into(), 400, CONTEXT_LENGTH_EXCEEDED),
            (anyhow::Error::new(UpstreamReadIdleTimeout { operation: "读取压缩摘要响应失败" }), 504, "compaction_timeout"),
            (anyhow::anyhow!("压缩摘要返回无效 JSON"), 502, "invalid_compaction_response"),
            (anyhow::anyhow!("上游未返回可用的压缩摘要"), 502, "invalid_compaction_response"),
            (UpstreamStreamFailure(json!({"type":"response.failed","response":{"error":{"status":429,"code":"rate_limit_exceeded","message":"limit sk-upstream","retry_after_ms":1234}}})).into(), 429, "rate_limit_exceeded"),
            (UpstreamStreamFailure(json!({"status":"failed","error":{"code":"invalid_api_key","message":"auth failed"}})).into(), 401, "invalid_prompt"),
            (UpstreamStreamFailure(json!({"type":"response.incomplete","response":{"output":[]}})).into(), 502, "invalid_compaction_response"),
        ] {
            let failure = gateway_compaction_response_failure(&error, route, phase, Some("req-sk-upstream"), None);
            assert_eq!(failure.status, status);
            let event = failure.normalized_event();
            let error = &event["response"]["error"];
            assert_eq!(error["code"], code);
            assert_eq!(error["codey"]["compactionPhase"], label);
            assert_eq!(error["codey"]["upstreamRequestId"], "req-***");
            assert_eq!(error["codey"]["routeId"], provider);
            assert!(error["message"].as_str().unwrap().contains(message));
            assert!(error["message"].as_str().unwrap().contains("Relay"));
            assert_eq!(error["message"].as_str().unwrap().contains("下次重试将尝试由源账号生成交接摘要"),
                code == CONTEXT_LENGTH_EXCEEDED && label == "target_route_handoff");
            assert!(!event.to_string().contains("sk-upstream"));
            if status == 429 { assert_eq!(error["codey"]["retryAfterMs"], 1234); }
        }
    }
}

#[tokio::test]
async fn gateway_compaction_http_200_response_failed_keeps_websocket_metadata_and_log() {
    let logs = tempfile::tempdir().unwrap();
    let value = json!({"type":"response.failed","response":{"status":"failed","error":{"status_code":429,"code":"rate_limit_exceeded","type":"rate_limit_error","message":"stream failure sk-upstream"}}});
    let (url, task, requests) = super::retry_contract_tests::error_upstream(
        200,
        format!("data: {value}\n\n"),
        "text/event-stream",
    )
    .await;
    let (mut config, provider, model) = router_config(url);
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
    let mut websocket = super::tests::connect_router_websocket(&router.endpoint()).await;
    websocket.send(WebSocketMessage::Text(json!({"type":"response.create","model":model_alias(&provider,&model),"input":[{"type":"compaction_trigger"}]}).to_string().into())).await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(message) = websocket.next().await {
            if let WebSocketMessage::Text(text) = message.unwrap() {
                let event: Value = serde_json::from_str(text.as_ref()).unwrap();
                if responses_event_is_terminal(&event) {
                    return event;
                }
            }
        }
        panic!("missing failure event")
    })
    .await
    .unwrap();
    let error = &event["response"]["error"];
    assert_eq!(error["code"], "rate_limit_exceeded");
    assert_eq!(error["codey"]["httpStatus"], 429);
    assert_eq!(error["codey"]["compactionPhase"], "gateway_summary");
    assert_eq!(error["codey"]["upstreamRequestId"], "retry-contract");
    assert_eq!(error["codey"]["upstreamType"], "rate_limit_error");
    assert!(error["codey"]["retryAfterMs"].as_u64().unwrap() <= 1000);
    assert!(!event.to_string().contains("sk-upstream"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    websocket.close(None).await.unwrap();
    router.stop().await.unwrap();
    task.abort();
    let page = crate::route_request_log::query_route_request_logs(
        logs.path(),
        crate::config::RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].upstream_status_code, Some(200));
    let summary = page.items[0].upstream_error_summary.as_deref().unwrap();
    assert!(summary.contains("stream failure ***"));
    assert!(!summary.contains("sk-upstream"));
}

#[tokio::test]
async fn gateway_compaction_handoff_failures_keep_json_and_stream_contracts() {
    let (config, provider, model) = router_config("http://127.0.0.1:9/v1".into());
    let snapshot = RouterSnapshot::from_config(&config);
    let resolved = RouteSelection {
        provider_id: provider.clone(),
        protocol: UpstreamProtocol::OpenAiResponses,
        requested_model: model_alias(&provider, &model),
        route: Arc::clone(&snapshot.routes[&provider]),
        upstream_model: model,
        fallback_reason: None,
    };
    for (phase, label) in [
        (
            GatewayCompactionPhase::SourceRouteHandoff,
            "source_route_handoff",
        ),
        (
            GatewayCompactionPhase::TargetRouteHandoff,
            "target_route_handoff",
        ),
    ] {
        for (embedded, context_exceeded) in [(false, false), (true, false), (false, true)] {
            for streaming in [false, true] {
                let status = if context_exceeded { 400 } else { 429 };
                let upstream_code = if context_exceeded {
                    CONTEXT_LENGTH_EXCEEDED
                } else {
                    "rate_limit_exceeded"
                };
                let upstream_type = if context_exceeded {
                    "invalid_request_error"
                } else {
                    "rate_limit_error"
                };
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let client = reqwest::Client::builder().no_proxy().build().unwrap();
                let request = client
                    .get(format!("http://{}/", listener.local_addr().unwrap()))
                    .send();
                let write = async {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    read_http_request(&mut socket).await.unwrap();
                    let mut downstream = HttpResponsesDownstream::new(socket);
                    downstream.set_streaming_errors(streaming);
                    let mut headers = HeaderMap::new();
                    headers.insert("retry-after", HeaderValue::from_static("2"));
                    let retry_advice = ResponseRetryAdvice::from_headers(&headers);
                    let error = json!({"error":{"code":upstream_code,"type":upstream_type,"message":"handoff failure sk-upstream"}});
                    if embedded {
                        write_gateway_compaction_response_error(
                            &mut downstream,
                            UpstreamStreamFailure(error).into(),
                            &resolved.route,
                            phase,
                            Some("handoff-request"),
                            retry_advice,
                        )
                        .await
                        .unwrap();
                    } else {
                        write_gateway_compaction_http_error(
                            &mut downstream,
                            UpstreamHttpErrorResponse {
                                status,
                                upstream_request_id: Some("handoff-request"),
                                retry_advice,
                                body: &serde_json::to_vec(&error).unwrap(),
                            },
                            &resolved,
                            ProtocolBridge::NativeResponses,
                            ResponsesRequestKind::Compact,
                            phase,
                        )
                        .await
                        .unwrap();
                    }
                };
                let (response, ()) = tokio::join!(request, write);
                let response = response.unwrap();
                assert_eq!(
                    response.status().as_u16(),
                    if streaming { 200 } else { status }
                );
                if streaming {
                    assert!(
                        response.headers()[CONTENT_TYPE]
                            .to_str()
                            .unwrap()
                            .starts_with("text/event-stream")
                    );
                } else {
                    assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
                    assert_eq!(response.headers()["retry-after"], "2");
                }
                let body = response.text().await.unwrap();
                assert!(!body.contains("sk-upstream"), "{body}");
                let error = if streaming {
                    let events = parse_responses_websocket_sse_events(&body).unwrap();
                    assert_eq!(events.last().unwrap()["type"], "response.failed");
                    events.last().unwrap()["response"]["error"].clone()
                } else {
                    serde_json::from_str::<Value>(&body).unwrap()["error"].clone()
                };
                assert_eq!(
                    error["code"],
                    if context_exceeded {
                        CONTEXT_LENGTH_EXCEEDED
                    } else if streaming {
                        "rate_limit_exceeded"
                    } else {
                        "upstream_http_error"
                    }
                );
                assert_eq!(error["codey"]["httpStatus"], status);
                assert_eq!(error["codey"]["compactionPhase"], label);
                assert_eq!(error["codey"]["routeId"], provider);
                assert_eq!(error["codey"]["upstreamCode"], upstream_code);
                assert_eq!(error["codey"]["upstreamType"], upstream_type);
                assert_eq!(error["codey"]["upstreamRequestId"], "handoff-request");
                assert_eq!(
                    error["message"]
                        .as_str()
                        .unwrap()
                        .contains("下次重试将尝试由源账号生成交接摘要"),
                    context_exceeded && label == "target_route_handoff"
                );
                if streaming && !context_exceeded {
                    assert!(error["codey"]["retryAfterMs"].as_u64().unwrap() <= 2000);
                }
            }
        }
    }
}

#[tokio::test]
async fn gateway_compaction_cancelled_errors_return_without_logging_or_writing() {
    struct UntouchedDownstream;
    impl ResponsesDownstream for UntouchedDownstream {
        fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
            panic!("cancelled requests must not be observed as upstream failures")
        }
        async fn write_error(
            &mut self,
            _: u16,
            _: &str,
            _: String,
            _: Option<&RouteTarget>,
        ) -> Result<()> {
            panic!("cancelled requests must not write a failure")
        }
        async fn write_json(&mut self, _: u16, _: &Value) -> Result<()> {
            panic!("unexpected JSON write")
        }
        async fn start_event_stream(&mut self) -> Result<()> {
            panic!("unexpected stream")
        }
        async fn write_event(&mut self, _: &Value) -> Result<()> {
            panic!("unexpected event")
        }
        async fn finish_event_stream(&mut self) -> Result<()> {
            panic!("unexpected stream finish")
        }
        async fn proxy_response_with_probe(
            &mut self,
            _: reqwest::Response,
            _: Option<&RouteRequestLogProbe>,
        ) -> Result<()> {
            panic!("unexpected proxy")
        }
    }
    let (config, provider, _) = router_config("http://127.0.0.1:9/v1".into());
    let snapshot = RouterSnapshot::from_config(&config);
    for closed in [true, false] {
        let error = if closed {
            anyhow::Error::new(DownstreamClosed)
        } else {
            anyhow::Error::new(DownstreamResponseInterrupt)
        }
        .context("original cancellation context");
        let returned = write_gateway_compaction_response_error(
            &mut UntouchedDownstream,
            error,
            &snapshot.routes[&provider],
            GatewayCompactionPhase::TargetRouteHandoff,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(returned.to_string(), "original cancellation context");
        assert_eq!(returned.is::<DownstreamClosed>(), closed);
        assert_eq!(returned.is::<DownstreamResponseInterrupt>(), !closed);
    }
}
