use super::*;

/// 原生压缩状态可能覆盖此前声明的工具作用域；普通生成请求需要在恢复状态后
/// 重放已有声明。只移动最后一次压缩之前的声明，不改工具定义或其它历史项。
pub(super) fn rehydrate_native_tool_manifest(body: &mut Value) -> bool {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(compaction_index) = items.iter().rposition(|item| item["type"] == "compaction") else {
        return false;
    };
    if !items[..compaction_index]
        .iter()
        .any(|item| item["type"] == "additional_tools")
    {
        return false;
    }

    let mut reordered = Vec::with_capacity(items.len());
    let mut declarations = Vec::new();
    for (index, item) in std::mem::take(items).into_iter().enumerate() {
        if index < compaction_index && item["type"] == "additional_tools" {
            declarations.push(item);
        } else {
            reordered.push(item);
            if index == compaction_index {
                reordered.append(&mut declarations);
            }
        }
    }
    *items = reordered;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_router::tests::{connect_router_websocket, router_config};

    fn manifest(id: &str) -> Value {
        json!({
            "type":"additional_tools", "id":id, "role":"developer", "tools":[{
                "type":"namespace", "name":"workspace", "description":"Local tools", "tools":[{
                    "type":"function", "name":"read_file", "defer_loading":true,
                    "parameters":{"type":"object", "properties":{"path":{"type":"string"}}}
                }, {
                    "type":"custom", "name":"patch", "format":{"type":"text"}
                }]
            }]
        })
    }

    fn compact(id: &str) -> Value {
        json!({"type":"compaction", "id":id, "encrypted_content":"opaque-window"})
    }

    #[test]
    fn tool_manifest_replay_preserves_definitions_and_all_other_items() {
        let declaration = manifest("at-current");
        let state = compact("cmp-current");
        let developer = json!({"role":"developer", "content":"Keep the task constraints"});
        let user = json!({"role":"user", "content":"Continue"});
        let top_tools =
            json!([{"type":"function", "name":"existing", "parameters":{"type":"object"}}]);
        let mut body = json!({"model":"model", "input":[declaration,developer,state,user],
                              "tools":top_tools, "tool_choice":"auto", "stream":true});
        assert!(rehydrate_native_tool_manifest(&mut body));
        assert_eq!(body["input"], json!([developer, state, declaration, user]));
        assert_eq!(body["tools"], top_tools);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["stream"], true);
        assert!(!rehydrate_native_tool_manifest(&mut body));
    }

    #[test]
    fn tool_manifest_replay_uses_last_compaction_and_keeps_late_updates() {
        let first = manifest("at-first");
        let update = manifest("at-update");
        let late =
            json!({"type":"additional_tools", "id":"at-late", "role":"developer", "tools":[]});
        let call = json!({"type":"function_call", "call_id":"call-1", "name":"read_file", "arguments":"{}"});
        let output = json!({"type":"function_call_output", "call_id":"call-1", "output":"kept"});
        let old = compact("cmp-old");
        let new = compact("cmp-new");
        let mut body = json!({"input":[first,old,call,update,output,new,late]});
        assert!(rehydrate_native_tool_manifest(&mut body));
        assert_eq!(
            body["input"],
            json!([old, call, output, new, first, update, late])
        );
        assert!(!rehydrate_native_tool_manifest(&mut body));
    }

    #[test]
    fn tool_manifest_replay_keeps_fresh_requests_and_already_replayed_history() {
        for mut body in [
            json!({"input":"plain user input"}),
            json!({"input":[manifest("at-fresh"),{"role":"user","content":"First turn"}]}),
            json!({"input":[compact("cmp"),manifest("at-after")]}),
            json!({"input":[compact("cmp"),{"type":"tool_search_output","call_id":"search","tools":[]}]}),
        ] {
            let original = body.clone();
            assert!(!rehydrate_native_tool_manifest(&mut body));
            assert_eq!(body, original);
        }
    }

    #[test]
    fn tool_manifest_replay_preserves_an_explicit_empty_declaration() {
        let declaration = json!({"type":"additional_tools","role":"developer","tools":[]});
        let state = compact("cmp");
        let mut body = json!({"input":[declaration,state]});
        assert!(rehydrate_native_tool_manifest(&mut body));
        assert_eq!(body["input"], json!([state, declaration]));
    }

    #[tokio::test]
    async fn tool_manifest_replay_reaches_small_and_large_native_http_requests() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for index in 0..5 {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let output = if index >= 3 {
                    json!([compact("cmp-result")])
                } else {
                    json!([])
                };
                write_json_response(
                    &mut socket,
                    200,
                    &json!({
                        "id":format!("resp-manifest-{index}"), "object":"response",
                        "status":"completed", "model":"provider-model", "output":output
                    }),
                )
                .await
                .unwrap();
                requests.push(body);
            }
            requests
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_remote_compaction = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::new();
        let declaration = manifest("at-current");
        let state = compact("cmp-current");
        let user = json!({"role":"user", "content":"Read the current file"});
        let inputs = [
            json!([declaration, user]),
            json!([declaration, state, user]),
            json!([declaration,state,{"role":"user","content":"x".repeat(REQUEST_JSON_OFFLOAD_BYTES + 1024)}]),
            json!([declaration,state,user,{"type":"compaction_trigger"}]),
            json!([declaration, state, user]),
        ];
        for (index, input) in inputs.iter().enumerate() {
            let path = if index == 4 {
                "responses/compact"
            } else {
                "responses"
            };
            let response = client
                .post(format!("{}/{path}", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(
                    &json!({"model":model_alias(&provider,&model), "input":input, "stream":false}),
                )
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status().as_u16(),
                200,
                "{}",
                response.text().await.unwrap()
            );
        }
        let forwarded = upstream_task.await.unwrap();
        router.stop().await.unwrap();
        assert_eq!(forwarded[0]["input"], inputs[0]);
        assert_eq!(forwarded[1]["input"], json!([state, declaration, user]));
        assert_eq!(forwarded[2]["input"][0], state);
        assert_eq!(forwarded[2]["input"][1], declaration);
        assert_eq!(forwarded[2]["input"][2], inputs[2][2]);
        assert_eq!(forwarded[3]["input"], inputs[3]);
        assert_eq!(forwarded[4]["input"], inputs[4]);
    }

    #[tokio::test]
    async fn tool_manifest_replay_skips_official_routes() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "id":"resp-official", "object":"response", "status":"completed", "output":[]
                }),
            )
            .await
            .unwrap();
            serde_json::from_slice::<Value>(&request.body).unwrap()
        });
        let (config, provider, model) = router_config(format!("http://{address}/v1"));
        let router = LocalRouter::start(&config).await.unwrap();
        let auth_home = tempfile::tempdir().unwrap();
        let auth_path = auth_home.path().join("auth.json");
        std::fs::write(&auth_path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"test-access","account_id":"test-account"}}"#
        ).unwrap();
        let mut snapshot = RouterSnapshot::from_config(&config);
        let route = Arc::make_mut(snapshot.routes.get_mut(&provider).unwrap());
        route.official_account = true;
        route.official_auth = Some(OfficialRouteAuth {
            account_id: "test-account".into(),
            email: None,
            record_path: auth_path.clone(),
            path: auth_path,
            siwc_path: None,
            accepts_incoming_authorization: false,
        });
        *router.snapshot.write().unwrap() = Arc::new(snapshot);
        let input = json!([manifest("at-official"),compact("cmp-official"),{"role":"user","content":"Read"}]);
        let endpoint = router.endpoint();
        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider,&model), "input":input, "stream":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            200,
            "{}",
            response.text().await.unwrap()
        );
        let forwarded = upstream_task.await.unwrap();
        router.stop().await.unwrap();
        assert_eq!(forwarded["input"], input);
    }

    #[tokio::test]
    async fn tool_manifest_replay_runs_after_websocket_http_history_expansion() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for index in 0..2 {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                requests.push(serde_json::from_slice::<Value>(&request.body).unwrap());
                write_json_response(
                    &mut stream,
                    200,
                    &json!({
                        "id":format!("resp-history-{index}"), "object":"response",
                        "status":"completed", "output":[]
                    }),
                )
                .await
                .unwrap();
            }
            requests
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = false;
        config.profiles[0].supports_remote_compaction = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let mut socket = connect_router_websocket(&router.endpoint()).await;
        let declaration = manifest("at-cached");
        let original = json!({"role":"user", "content":"Original task"});
        let state = compact("cmp-later");
        let next = json!({"role":"user", "content":"Continue after compaction"});
        for (index, input) in [json!([declaration, original]), json!([state, next])]
            .into_iter()
            .enumerate()
        {
            let mut body = json!({"type":"response.create", "model":model_alias(&provider,&model), "input":input});
            if index == 1 {
                body["previous_response_id"] = json!("resp-history-0");
            }
            socket
                .send(WebSocketMessage::Text(body.to_string().into()))
                .await
                .unwrap();
            let terminal = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() {
                        let event: Value = serde_json::from_str(text.as_str()).unwrap();
                        if responses_event_is_terminal(&event) {
                            break event;
                        }
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(terminal["type"], "response.completed", "{terminal}");
            assert_eq!(terminal["response"]["id"], format!("resp-history-{index}"));
        }
        socket.close(None).await.unwrap();
        router.stop().await.unwrap();
        let forwarded = upstream_task.await.unwrap();
        assert_eq!(forwarded[0]["input"], json!([declaration, original]));
        assert!(forwarded[1].get("previous_response_id").is_none());
        assert_eq!(
            forwarded[1]["input"],
            json!([original, state, declaration, next])
        );
    }

    #[tokio::test]
    async fn tool_manifest_audit_large_zstd_sse_requests() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            assert!(incoming_header(&request, "content-encoding").is_none());
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let event = json!({"type":"response.completed", "response":{
                "id":"resp-manifest-sse", "object":"response", "status":"completed",
                "output":[{"type":"message", "role":"assistant", "content":[
                    {"type":"output_text", "text":"manifest-sse-ok"}
                ]}]
            }});
            write_static_response(
                &mut stream,
                "text/event-stream",
                format!("data: {event}\n\n").as_bytes(),
            )
            .await
            .unwrap();
            body
        });
        let (config, provider, model) = router_config(format!("http://{address}/v1"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let declaration = manifest("at-compressed");
        let late = manifest("at-late");
        let state = compact("cmp-compressed");
        let user = json!({"role":"user", "content":"x".repeat(REQUEST_JSON_OFFLOAD_BYTES + 1024)});
        let top_tools =
            json!([{"type":"function", "name":"top_level", "parameters":{"type":"object"}}]);
        let body = json!({"model":model_alias(&provider,&model),
            "input":[declaration,state,late,user], "tools":top_tools, "stream":true});
        let raw = serde_json::to_vec(&body).unwrap();
        assert!(raw.len() > REQUEST_JSON_OFFLOAD_BYTES);
        let compressed = zstd::stream::encode_all(Cursor::new(raw), 3).unwrap();
        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header(CONTENT_TYPE, "application/json")
            .header("content-encoding", "zstd")
            .body(compressed)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert!(is_sse_content_type(
            response.headers()[CONTENT_TYPE].to_str().unwrap()
        ));
        let response = response.text().await.unwrap();
        assert!(response.contains("manifest-sse-ok"), "{response}");
        assert!(response.contains("response.completed"), "{response}");
        let forwarded = upstream_task.await.unwrap();
        router.stop().await.unwrap();
        assert_eq!(forwarded["input"], json!([state, declaration, late, user]));
        assert_eq!(forwarded["tools"], top_tools);
        assert_eq!(forwarded["stream"], true);
    }

    #[tokio::test]
    async fn tool_manifest_audit_reused_native_upstream_websocket() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut requests = Vec::new();
            for index in 0..2 {
                let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected native upstream request");
                };
                requests.push(serde_json::from_str::<Value>(text.as_str()).unwrap());
                socket
                    .send(WebSocketMessage::Text(
                        json!({
                            "type":"response.completed", "response":{
                                "id":format!("resp-manifest-ws-{index}"), "object":"response",
                                "status":"completed", "output":[]
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            requests
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = true;
        config.profiles[0].supports_remote_compaction = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let mut socket = connect_router_websocket(&router.endpoint()).await;
        let declaration = manifest("at-first");
        let update = manifest("at-update");
        let late =
            json!({"type":"additional_tools", "id":"at-clear", "role":"developer", "tools":[]});
        let old = compact("cmp-first");
        let new = compact("cmp-last");
        let user = json!({"role":"user", "content":"First turn"});
        let next = json!({"role":"user", "content":"Next turn"});
        let call = json!({"type":"custom_tool_call", "call_id":"call-patch", "name":"patch", "input":"test"});
        let output =
            json!({"type":"custom_tool_call_output", "call_id":"call-patch", "output":"done"});
        let inputs = [
            json!([declaration, old, user]),
            json!([
                declaration,
                old,
                user,
                call,
                output,
                update,
                new,
                next,
                late
            ]),
        ];
        for (index, input) in inputs.into_iter().enumerate() {
            socket.send(WebSocketMessage::Text(json!({
                "type":"response.create", "model":model_alias(&provider,&model), "input":input
            }).to_string().into())).await.unwrap();
            let terminal = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() {
                        let event: Value = serde_json::from_str(text.as_str()).unwrap();
                        if responses_event_is_terminal(&event) {
                            break event;
                        }
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(terminal["type"], "response.completed", "{terminal}");
            assert_eq!(
                terminal["response"]["id"],
                format!("resp-manifest-ws-{index}")
            );
        }
        socket.close(None).await.unwrap();
        router.stop().await.unwrap();
        let forwarded = upstream_task.await.unwrap();
        assert_eq!(forwarded[0]["input"], json!([old, declaration, user]));
        assert_eq!(
            forwarded[1]["input"],
            json!([
                old,
                user,
                call,
                output,
                new,
                declaration,
                update,
                next,
                late
            ])
        );
        assert!(forwarded.iter().all(|body| body.get("stream").is_none()));
    }

    #[tokio::test]
    async fn tool_manifest_full_review_preserves_retry_and_compaction_errors() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let rejected_id = format!("rs_manifest_retry_{}", Uuid::new_v4().simple());
        let upstream_id = rejected_id.clone();
        let upstream_task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for index in 0..3 {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                requests.push(serde_json::from_slice::<Value>(&request.body).unwrap());
                let (status, response) = match index {
                    0 => (
                        400,
                        json!({"error":{
                            "type":"invalid_request_error", "code":"invalid_encrypted_content",
                            "message":format!("The encrypted content for item {upstream_id} could not be verified.")
                        }}),
                    ),
                    1 => (
                        200,
                        json!({"id":"resp-manifest-repaired", "object":"response",
                        "status":"completed", "output":[]}),
                    ),
                    _ => (
                        400,
                        json!({"error":{
                            "type":"invalid_request_error", "code":"invalid_compaction",
                            "message":"invalid compact state; original history required"
                        }}),
                    ),
                };
                write_json_response(&mut stream, status, &response)
                    .await
                    .unwrap();
            }
            requests
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_remote_compaction = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::new();
        let declaration = manifest("at-retry");
        let state = compact("cmp-retry");
        let user = json!({"role":"user", "content":"Continue"});
        let summary = json!([{"type":"summary_text", "text":"Keep the visible reasoning"}]);
        let reasoning = json!({"type":"reasoning", "id":rejected_id,
            "encrypted_content":"rejected-reasoning", "summary":summary});
        let response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider,&model),
                "input":[declaration,state,reasoning,user], "stream":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap()["id"],
            "resp-manifest-repaired"
        );

        let invalid_state = compact("cmp-invalid");
        let response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider,&model),
                "input":[declaration,invalid_state,user], "stream":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 400);
        // Ordinary non-stream failures keep the existing plain-text contract.
        assert!(
            response.headers()[CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
        let error = response.text().await.unwrap();
        assert!(error.contains("invalid_compaction"), "{error}");
        assert!(error.contains("original history required"), "{error}");
        let forwarded = upstream_task.await.unwrap();
        router.stop().await.unwrap();
        assert_eq!(
            forwarded.len(),
            3,
            "Only the rejected reasoning request may retry"
        );
        assert_eq!(
            forwarded[0]["input"],
            json!([state, declaration, reasoning, user])
        );
        let repaired = json!({"type":"reasoning", "summary":summary});
        assert_eq!(
            forwarded[1]["input"],
            json!([state, declaration, repaired, user])
        );
        assert_eq!(
            forwarded[2]["input"],
            json!([invalid_state, declaration, user])
        );
    }
}
