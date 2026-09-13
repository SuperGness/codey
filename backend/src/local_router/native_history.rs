use super::*;

const NATIVE_HISTORY_CACHE_BYTES: usize = 16 * 1024 * 1024;
const NATIVE_HISTORY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

struct NativeHistoryEntry {
    scope: [u8; 32],
    owner: [u8; 32],
    expires_at: Instant,
    history: Arc<AdaptedResponsesHistory>,
}

#[derive(Default)]
pub(crate) struct NativeHistoryCache {
    entries: VecDeque<NativeHistoryEntry>,
    bytes: usize,
}

impl NativeHistoryCache {
    fn prune(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|entry| entry.expires_at <= now)
        {
            self.remove_oldest();
        }
    }

    fn remove_oldest(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            self.bytes -= entry.history.retained_bytes();
        }
    }

    fn get(
        &mut self,
        scope: [u8; 32],
        owner: [u8; 32],
        id: &str,
    ) -> Option<Arc<AdaptedResponsesHistory>> {
        self.prune(Instant::now());
        // ponytail: scan at most 64 snapshots; use a map if this bound grows.
        self.entries
            .iter()
            .rev()
            .find(|entry| {
                entry.scope == scope
                    && entry.owner == owner
                    && entry
                        .history
                        .last
                        .as_ref()
                        .is_some_and(|(response_id, _)| response_id == id)
            })
            .map(|entry| Arc::clone(&entry.history))
    }

    fn insert(&mut self, scope: [u8; 32], owner: [u8; 32], history: Arc<AdaptedResponsesHistory>) {
        let now = Instant::now();
        self.prune(now);
        let bytes = history.retained_bytes();
        if bytes > NATIVE_HISTORY_CACHE_BYTES {
            return;
        }
        while self.entries.len() >= MAX_CONCURRENT_CONNECTIONS
            || self.bytes.saturating_add(bytes) > NATIVE_HISTORY_CACHE_BYTES
        {
            self.remove_oldest();
        }
        self.bytes += bytes;
        self.entries.push_back(NativeHistoryEntry {
            scope,
            owner,
            expires_at: now + NATIVE_HISTORY_CACHE_TTL,
            history,
        });
    }
}

#[derive(Default)]
pub(crate) struct NativeResponsesHistory {
    owner: Option<[u8; 32]>,
    scope: Option<[u8; 32]>,
    cache: Arc<Mutex<NativeHistoryCache>>,
    latest: Option<Arc<AdaptedResponsesHistory>>,
    history: AdaptedResponsesHistory,
    unavailable: Option<String>,
}

pub(crate) fn native_history_key(
    route: &RouteTarget,
    auth: UpstreamWebSocketAuthIdentity,
    body: &Value,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(route.context_config);
    update_length_prefixed_digest(&mut digest, route.provider_id.as_bytes());
    update_length_prefixed_digest(
        &mut digest,
        body.get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .as_bytes(),
    );
    for identity in [auth.authorization, auth.account_id] {
        digest.update([u8::from(identity.is_some())]);
        if let Some(identity) = identity {
            digest.update(identity);
        }
    }
    digest.finalize().into()
}

impl NativeResponsesHistory {
    pub(crate) fn with_cache(
        cache: Arc<Mutex<NativeHistoryCache>>,
        headers: &[(String, String)],
    ) -> Self {
        let scope = ["thread-id", "session-id"].into_iter().find_map(|name| {
            headers
                .iter()
                .find(|(header, _)| header.eq_ignore_ascii_case(name))
                .and_then(|(_, value)| valid_codex_session_id(value))
                .map(|value| Sha256::digest(value.as_bytes()).into())
        });
        Self {
            cache,
            scope,
            ..Self::default()
        }
    }

    pub(crate) fn has_history(&self) -> bool {
        self.latest.is_some()
    }

    pub(crate) fn clear_pending(&mut self) {
        self.history.clear_pending();
    }

    pub(crate) fn prepare(&mut self, owner: [u8; 32], body: &mut Value) {
        if self.owner != Some(owner) {
            self.history = AdaptedResponsesHistory::default();
            self.latest = None;
            self.owner = Some(owner);
        }
        let previous = responses_previous_response_id(body).and_then(|id| {
            self.latest
                .as_ref()
                .filter(|history| {
                    history
                        .last
                        .as_ref()
                        .is_some_and(|(response_id, _)| response_id == id)
                })
                .cloned()
                .or_else(|| {
                    self.scope.and_then(|scope| {
                        self.cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .get(scope, owner, id)
                    })
                })
        });
        // A cache miss must not prevent a healthy native WS continuation.
        // Require complete history when reopening WS or falling back to HTTP.
        self.unavailable = self
            .history
            .stage_native(body, previous.as_deref())
            .err()
            .map(|error| error.to_string());
        if self.unavailable.is_some() {
            self.history.clear_pending();
        }
    }

    pub(crate) fn restore(&mut self, owner: [u8; 32], body: &mut Value) -> Result<bool> {
        if responses_previous_response_id(body).is_none() {
            if let Some(input) = body.get("input").and_then(Value::as_array) {
                validate_native_tool_history(input, false)?;
            }
            return Ok(false);
        }
        if self.owner != Some(owner) {
            anyhow::bail!("上游身份已变化，无法安全恢复续接历史；请重新发送完整上下文");
        }
        if let Some(reason) = &self.unavailable {
            anyhow::bail!("无法恢复原生续接历史：{reason}；请重新发送完整上下文");
        }
        let input = self
            .history
            .pending_input
            .as_ref()
            .context("缺少完整续接历史，请重新发送完整上下文")?;
        validate_native_tool_history(input, true)?;
        let object = body
            .as_object_mut()
            .context("Responses 请求必须是 JSON 对象")?;
        object.insert("input".into(), Value::Array(input.clone()));
        object.remove("previous_response_id");
        Ok(true)
    }

    pub(crate) fn observe(&mut self, event: &Value) {
        if self.history.pending_input.is_none() || !responses_event_is_terminal(event) {
            return;
        }
        let response = &event["response"];
        let complete = event.get("type").and_then(Value::as_str) == Some("response.completed")
            && response
                .get("status")
                .and_then(Value::as_str)
                .is_none_or(|status| status == "completed");
        let id = response
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 1024);
        if complete
            && let (Some(id), Some(output)) = (id, response.get("output").and_then(Value::as_array))
        {
            // Missing/oversized terminal output makes recovery unavailable;
            // it must not turn an already delivered generation into a retry.
            self.unavailable = self
                .history
                .remember(id, output)
                .err()
                .map(|error| error.to_string());
        }
        self.history.clear_pending();
        if self.history.last.is_some() {
            let history = Arc::new(std::mem::take(&mut self.history));
            if let (Some(scope), Some(owner)) = (self.scope, self.owner) {
                self.cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(scope, owner, Arc::clone(&history));
            }
            self.latest = Some(history);
        }
    }
}

fn validate_native_tool_history(input: &[Value], restoring: bool) -> Result<()> {
    let mut calls = HashSet::new();
    for item in input {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        // compaction_trigger is a request instruction, not a history reference.
        match kind {
            "item_reference" | "compaction" if restoring => {
                anyhow::bail!("历史包含无法在协议切换时展开的引用，请重新发送完整上下文");
            }
            "reasoning"
                if restoring
                    && item
                        .get("encrypted_content")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty) =>
            {
                anyhow::bail!("推理历史缺少可恢复内容，请重新发送完整上下文");
            }
            "function_call" | "custom_tool_call" => {
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .context("工具调用缺少 call_id，请重新发送完整上下文")?;
                if !calls.insert((kind, id)) {
                    anyhow::bail!("历史中工具调用 ID 重复，请重新发送完整上下文");
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_kind = if kind == "function_call_output" {
                    "function_call"
                } else {
                    "custom_tool_call"
                };
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !calls.remove(&(call_kind, id)) {
                    anyhow::bail!("工具结果缺少对应的完整调用历史，请重新发送完整上下文");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        connect_router_websocket, connect_router_websocket_with_headers, router_config,
    };
    use super::*;

    fn call(custom: bool, id: &str) -> Value {
        if custom {
            json!({"type":"custom_tool_call","call_id":id,"name":"run","input":"pwd"})
        } else {
            json!({"type":"function_call","call_id":id,"name":"run","arguments":"{}"})
        }
    }

    fn result(custom: bool, id: &str) -> Value {
        json!({"type":if custom {"custom_tool_call_output"} else {"function_call_output"},"call_id":id,"output":"done"})
    }

    fn completed(id: &str, output: Vec<Value>) -> Value {
        json!({"type":"response.completed","response":{"id":id,"object":"response","status":"completed","output":output}})
    }

    async fn terminal(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let message = socket.next().await.unwrap().unwrap();
                if let WebSocketMessage::Text(text) = message {
                    let event: Value = serde_json::from_str(&text).unwrap();
                    if responses_event_is_terminal(&event) {
                        return event;
                    }
                }
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn reconnected_native_ws_restores_tool_rounds_and_older_branch_over_http_sse_and_json() {
        for custom in [false, true] {
            for sse in [false, true] {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let address = listener.local_addr().unwrap();
                let (closed_tx, closed_rx) = oneshot::channel();
                let upstream = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                    let first = ws.next().await.unwrap().unwrap();
                    assert!(matches!(first, WebSocketMessage::Text(_)));
                    ws.send(WebSocketMessage::Text(
                        completed("resp-first", vec![call(custom, "call-1")])
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                    ws.close(None).await.unwrap();
                    // Wait until Codey has consumed the upstream Close and
                    // released its socket before the client submits the result.
                    let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
                        .await
                        .unwrap();
                    closed_tx.send(()).unwrap();
                    // The fresh WS handshake fails before any model request is sent.
                    // HTTP must receive the expanded body, never the old encoded delta.
                    let (mut rejected, _) = listener.accept().await.unwrap();
                    assert_eq!(
                        read_http_request(&mut rejected).await.unwrap().method,
                        "GET"
                    );
                    rejected.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();
                    drop(rejected);
                    for round in 1..=3 {
                        let (mut socket, _) =
                            tokio::time::timeout(Duration::from_secs(5), listener.accept())
                                .await
                                .unwrap()
                                .unwrap();
                        let request = read_http_request(&mut socket).await.unwrap();
                        assert_eq!(request.method, "POST");
                        let body: Value = serde_json::from_slice(&request.body).unwrap();
                        assert!(body.get("previous_response_id").is_none());
                        assert_eq!(body["stream"], true);
                        assert_eq!(body["instructions"], "keep this instruction");
                        let mut expected = vec![
                            json!({"role":"user","content":"original task"}),
                            call(custom, "call-1"),
                            result(custom, "call-1"),
                        ];
                        if round == 2 {
                            expected.extend([call(custom, "call-2"), result(custom, "call-2")]);
                        }
                        assert_eq!(body["input"], Value::Array(expected));
                        let event = completed(
                            if round == 1 {
                                "resp-second"
                            } else {
                                "resp-final"
                            },
                            if round == 1 {
                                vec![call(custom, "call-2")]
                            } else {
                                vec![]
                            },
                        );
                        let response = if sse {
                            format!("data: {event}\n\n")
                        } else {
                            event["response"].to_string()
                        };
                        let content_type = if sse {
                            "text/event-stream"
                        } else {
                            "application/json"
                        };
                        socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
                    }
                });
                let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
                config.profiles[0].supports_websockets = true;
                let router = LocalRouter::start(&config).await.unwrap();
                let mut client = connect_router_websocket_with_headers(
                    &router.endpoint(),
                    &[("session-id", "history-session")],
                )
                .await;
                let model = model_alias(&provider, &model);
                client
                    .send(WebSocketMessage::Text(
                        json!({"type":"response.create","model":model,"input":"original task"})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                assert_eq!(terminal(&mut client).await["response"]["id"], "resp-first");
                closed_rx.await.unwrap();
                client.close(None).await.unwrap();
                let mut client = connect_router_websocket_with_headers(
                    &router.endpoint(),
                    &[("session-id", "history-session")],
                )
                .await;
                for (id, call_id) in [
                    ("resp-first", "call-1"),
                    ("resp-second", "call-2"),
                    ("resp-first", "call-1"),
                ] {
                    client.send(WebSocketMessage::Text(json!({"type":"response.create","model":model,"previous_response_id":id,"input":[result(custom, call_id)],"instructions":"keep this instruction"}).to_string().into())).await.unwrap();
                    assert_eq!(terminal(&mut client).await["type"], "response.completed");
                }
                upstream.await.unwrap();
                client.close(None).await.unwrap();
                router.stop().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn recovered_history_reconnects_ws_then_returns_to_incremental_requests() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = oneshot::channel();
        let reasoning = json!({
            "type":"reasoning", "encrypted_content":"encrypted-test-history",
            "content":[{"type":"reasoning_text","text":"Check before calling the tool."}]
        });
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut first = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert!(matches!(
                first.next().await.unwrap().unwrap(),
                WebSocketMessage::Text(_)
            ));
            first
                .send(WebSocketMessage::Text(
                    completed("resp-first", vec![reasoning.clone(), call(false, "call-1")])
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            first.close(None).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(3), first.next())
                .await
                .unwrap();
            closed_tx.send(()).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut recovered = tokio_tungstenite::accept_async(stream).await.unwrap();
            for round in 1..=2 {
                let WebSocketMessage::Text(text) = recovered.next().await.unwrap().unwrap() else {
                    panic!("expected response.create");
                };
                let body: Value = serde_json::from_str(&text).unwrap();
                if round == 1 {
                    assert!(body.get("previous_response_id").is_none());
                    assert_eq!(
                        body["input"],
                        json!([
                            {"role":"user","content":"original task"}, reasoning,
                            call(false,"call-1"), result(false,"call-1")
                        ])
                    );
                } else {
                    assert_eq!(body["previous_response_id"], "resp-second");
                    assert_eq!(body["input"], json!([result(false, "call-2")]));
                }
                assert_eq!(body["instructions"], "keep this instruction");
                recovered
                    .send(WebSocketMessage::Text(
                        completed(
                            if round == 1 {
                                "resp-second"
                            } else {
                                "resp-final"
                            },
                            if round == 1 {
                                vec![call(false, "call-2")]
                            } else {
                                vec![]
                            },
                        )
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let model = model_alias(&provider, &model);
        let mut client =
            connect_router_websocket_with_headers(&router.endpoint(), &[("thread-id", "task")])
                .await;
        client
            .send(WebSocketMessage::Text(
                json!({"type":"response.create","model":model,"input":"original task"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        assert_eq!(terminal(&mut client).await["response"]["id"], "resp-first");
        closed_rx.await.unwrap();
        client.close(None).await.unwrap();
        let mut client =
            connect_router_websocket_with_headers(&router.endpoint(), &[("thread-id", "task")])
                .await;
        for (id, call_id) in [("resp-first", "call-1"), ("resp-second", "call-2")] {
            client.send(WebSocketMessage::Text(json!({"type":"response.create","model":model,"previous_response_id":id,"input":[result(false,call_id)],"instructions":"keep this instruction"}).to_string().into())).await.unwrap();
            assert_eq!(terminal(&mut client).await["type"], "response.completed");
        }
        upstream.await.unwrap();
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn websocket_compaction_restores_history_before_switching_to_http() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider, model) =
            router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
        config.profiles[0].supports_websockets = true;
        config.profiles[0].supports_remote_compaction = true;
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            socket
                .send(WebSocketMessage::Text(
                    completed("resp-first", vec![]).to_string().into(),
                ))
                .await
                .unwrap();
            let (mut http, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut http).await.unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body.get("previous_response_id").is_none(), "{body}");
            assert_eq!(
                body["input"],
                json!([{"role":"user","content":"original task"},{"type":"compaction_trigger"}])
            );
            write_json_response(
                &mut http,
                200,
                &completed(
                    "resp-compact",
                    vec![json!({"type":"compaction","encrypted_content":"opaque"})],
                )["response"],
            )
            .await
            .unwrap();
        });
        let router = LocalRouter::start(&config).await.unwrap();
        let mut client = connect_router_websocket(&router.endpoint()).await;
        let model = model_alias(&provider, &model);
        client
            .send(WebSocketMessage::Text(
                json!({"type":"response.create","model":model,"input":"original task"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        assert_eq!(terminal(&mut client).await["response"]["id"], "resp-first");
        client.send(WebSocketMessage::Text(json!({"type":"response.create","model":model,"previous_response_id":"resp-first","input":[{"type":"compaction_trigger"}]}).to_string().into())).await.unwrap();
        assert_eq!(
            terminal(&mut client).await["response"]["id"],
            "resp-compact"
        );
        upstream.await.unwrap();
        client.send(WebSocketMessage::Text(json!({"type":"response.create","model":model,"previous_response_id":"resp-missing","input":[{"type":"compaction_trigger"}]}).to_string().into())).await.unwrap();
        assert_eq!(
            terminal(&mut client).await["response"]["error"]["code"],
            "context_not_recoverable"
        );
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_native_tool_continuation_never_reaches_upstream() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider, model) =
            router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let mut client = connect_router_websocket(&router.endpoint()).await;
        for custom in [false, true] {
            client.send(WebSocketMessage::Text(json!({"type":"response.create","model":model_alias(&provider, &model),"previous_response_id":"resp-from-another-socket","input":[result(custom, "missing-call")]}).to_string().into())).await.unwrap();
            let error = terminal(&mut client).await;
            assert_eq!(
                error["response"]["error"]["code"],
                "context_not_recoverable"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
    }

    #[test]
    fn native_history_rejects_missing_output_identity_changes_and_orphan_results() {
        for custom in [false, true] {
            let mut history = NativeResponsesHistory::default();
            let key = [1; 32];
            history.prepare(key, &mut json!({"input":"task"}));
            history.observe(&completed("resp-known", vec![call(custom, "call-1")]));
            let mut next =
                json!({"previous_response_id":"resp-known","input":[result(custom, "call-1")]});
            history.prepare(key, &mut next);
            assert!(history.restore([2; 32], &mut next).is_err());
            assert_eq!(next["previous_response_id"], "resp-known");
            assert!(history.restore(key, &mut next).unwrap());
            assert_eq!(next["input"].as_array().unwrap().len(), 3);
            history.observe(&json!({"type":"response.completed","response":{"id":"resp-no-output","status":"completed"}}));
            let mut next =
                json!({"previous_response_id":"resp-no-output","input":[result(custom, "call-1")]});
            history.prepare(key, &mut next);
            assert!(history.restore(key, &mut next).is_err());
            let mut orphan = json!({"input":[result(custom, "missing-call")]});
            history.prepare(key, &mut orphan);
            assert!(history.restore(key, &mut orphan).is_err());
        }
    }

    #[test]
    fn shared_native_history_is_scoped_and_expires_without_retaining_snapshots() {
        let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
        let headers = vec![("session-id".into(), "task-a".into())];
        let mut first = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
        first.prepare([1; 32], &mut json!({"input":"task"}));
        first.observe(&completed("resp-shared", vec![call(false, "call-1")]));
        let weak = Arc::downgrade(first.latest.as_ref().unwrap());
        drop(first);
        for (headers, owner, allowed) in [
            (headers.clone(), [1; 32], true),
            (vec![("session-id".into(), "task-b".into())], [1; 32], false),
            (vec![], [1; 32], false),
            (
                vec![
                    ("thread-id".into(), "task-b".into()),
                    ("session-id".into(), "task-a".into()),
                ],
                [1; 32],
                false,
            ),
            (headers.clone(), [2; 32], false),
        ] {
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
            let mut body =
                json!({"previous_response_id":"resp-shared","input":[result(false,"call-1")]});
            history.prepare(owner, &mut body);
            assert_eq!(history.restore(owner, &mut body).is_ok(), allowed);
            if allowed {
                assert_eq!(
                    body["input"],
                    json!([
                        {"role":"user","content":"task"}, call(false,"call-1"), result(false,"call-1")
                    ])
                );
            } else {
                assert_eq!(body["previous_response_id"], "resp-shared");
                assert_eq!(body["input"].as_array().unwrap().len(), 1);
            }
        }
        cache
            .lock()
            .unwrap()
            .prune(Instant::now() + NATIVE_HISTORY_CACHE_TTL);
        assert_eq!(cache.lock().unwrap().bytes, 0);
        assert!(weak.upgrade().is_none());
        let mut expired = NativeResponsesHistory::with_cache(cache, &headers);
        let mut body =
            json!({"previous_response_id":"resp-shared","input":[result(false,"call-1")]});
        expired.prepare([1; 32], &mut body);
        assert!(expired.restore([1; 32], &mut body).is_err());
    }

    #[test]
    fn shared_native_history_bounds_snapshot_count_and_bytes() {
        let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
        let mut history = NativeResponsesHistory::with_cache(
            Arc::clone(&cache),
            &[("session-id".into(), "task".into())],
        );
        for index in 0..=MAX_CONCURRENT_CONNECTIONS {
            history.prepare([1; 32], &mut json!({"input":"task"}));
            history.observe(&completed(&format!("resp-{index}"), vec![]));
        }
        let scope = history.scope.unwrap();
        assert_eq!(
            cache.lock().unwrap().entries.len(),
            MAX_CONCURRENT_CONNECTIONS
        );
        assert!(
            cache
                .lock()
                .unwrap()
                .get(scope, [1; 32], "resp-0")
                .is_none()
        );
        for index in 0..3 {
            history.prepare([1; 32], &mut json!({"input":"task"}));
            history.observe(&completed(
                &format!("large-{index}"),
                vec![json!({
                    "role":"assistant", "content":"x".repeat(6 * 1024 * 1024)
                })],
            ));
            assert!(history.unavailable.is_none());
        }
        let mut cache = cache.lock().unwrap();
        assert!(cache.bytes <= NATIVE_HISTORY_CACHE_BYTES);
        assert!(cache.get(scope, [1; 32], "large-0").is_none());
        assert!(cache.get(scope, [1; 32], "large-1").is_some());
        assert!(cache.get(scope, [1; 32], "large-2").is_some());
    }

    #[test]
    fn native_history_identity_ignores_ws_toggle_but_tracks_model_and_credentials() {
        let (mut config, provider, model) = router_config("http://127.0.0.1:9/v1".into());
        config.profiles[0].supports_websockets = true;
        let initial = RouterSnapshot::from_config(&config);
        let body = json!({"model":model});
        let auth = UpstreamWebSocketAuthIdentity::default();
        let key = native_history_key(&initial.routes[&provider], auth, &body);
        config.profiles[0].supports_websockets = false;
        let updated = RouterSnapshot::from_config(&config);
        assert_eq!(
            key,
            native_history_key(&updated.routes[&provider], auth, &body)
        );
        assert_ne!(
            key,
            native_history_key(&updated.routes[&provider], auth, &json!({"model":"other"}))
        );
        let other_auth = UpstreamWebSocketAuthIdentity {
            account_id: Some([9; 32]),
            ..auth
        };
        assert_ne!(
            key,
            native_history_key(&updated.routes[&provider], other_auth, &body)
        );
        config.profiles[0]
            .model_request_headers
            .insert("x-tenant".into(), "another-tenant".into());
        let changed = RouterSnapshot::from_config(&config);
        assert_ne!(
            key,
            native_history_key(&changed.routes[&provider], auth, &body)
        );
    }
}
