use super::*;

const NATIVE_HISTORY_CACHE_BYTES: usize = 16 * 1024 * 1024;
const NATIVE_HISTORY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
// Entries hold whole session snapshots, so this bound counts conversations
// rather than history items inside one request.
const NATIVE_HISTORY_CACHE_ENTRIES: usize = 64;

struct NativeHistoryEntry {
    scope: [u8; 32],
    owner: [u8; 32],
    expires_at: Instant,
    history: Arc<AdaptedResponsesHistory>,
    force_http: bool,
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
    ) -> Option<(Arc<AdaptedResponsesHistory>, bool)> {
        self.prune(Instant::now());
        // Linear scan of at most NATIVE_HISTORY_CACHE_ENTRIES snapshots; use a
        // map if this bound grows.
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
            .map(|entry| (Arc::clone(&entry.history), entry.force_http))
    }

    fn mark_requires_http(&mut self, scope: [u8; 32], owner: [u8; 32], id: [u8; 32]) {
        self.prune(Instant::now());
        for entry in &mut self.entries {
            if entry.scope == scope
                && entry.owner == owner
                && entry.history.last.as_ref().is_some_and(|(response_id, _)| {
                    <[u8; 32]>::from(Sha256::digest(response_id.as_bytes())) == id
                })
            {
                entry.force_http = true;
            }
        }
    }

    fn insert(&mut self, scope: [u8; 32], owner: [u8; 32], history: Arc<AdaptedResponsesHistory>) {
        let now = Instant::now();
        self.prune(now);
        let bytes = history.retained_bytes();
        if bytes > NATIVE_HISTORY_CACHE_BYTES {
            return;
        }
        while self.entries.len() >= NATIVE_HISTORY_CACHE_ENTRIES
            || self.bytes.saturating_add(bytes) > NATIVE_HISTORY_CACHE_BYTES
        {
            self.remove_oldest();
        }
        self.bytes += bytes;
        self.entries.push_back(NativeHistoryEntry {
            scope,
            owner,
            expires_at: now + NATIVE_HISTORY_CACHE_TTL,
            force_http: history.requires_native_http,
            history,
        });
    }
}

#[derive(Default)]
pub(crate) struct NativeResponsesHistory {
    owner: Option<[u8; 32]>,
    refresh_scope: Option<[u8; 32]>,
    force_http: bool,
    pending_previous_response: Option<[u8; 32]>,
    scope: Option<[u8; 32]>,
    cache: Arc<Mutex<NativeHistoryCache>>,
    latest: Option<Arc<AdaptedResponsesHistory>>,
    latest_requires_http: bool,
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
    pub(crate) fn prepare_for_route(
        &mut self,
        route: &RouteTarget,
        auth: UpstreamWebSocketAuthIdentity,
        body: &mut Value,
    ) -> [u8; 32] {
        let owner = native_history_key(route, auth, body);
        let refresh_scope =
            (route.official_account && auth.authorization.is_some() && auth.account_id.is_some())
                .then(|| {
                    native_history_key(
                        route,
                        UpstreamWebSocketAuthIdentity {
                            authorization: None,
                            ..auth
                        },
                        body,
                    )
                });
        if refresh_scope.is_some()
            && self.refresh_scope == refresh_scope
            && responses_previous_response_id(body).is_some_and(|response_id| {
                self.latest.as_ref().is_some_and(|history| {
                    history
                        .last
                        .as_ref()
                        .is_some_and(|(id, _)| id == response_id)
                })
            })
        {
            self.owner = Some(owner);
        }
        self.prepare(owner, body);
        self.refresh_scope = refresh_scope;
        owner
    }

    pub(crate) fn requires_http(&self) -> bool {
        self.force_http || self.history.requires_native_http
    }

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
        self.pending_previous_response = None;
    }

    pub(crate) fn prepare(&mut self, owner: [u8; 32], body: &mut Value) {
        if self.owner != Some(owner) {
            self.history = AdaptedResponsesHistory::default();
            self.latest = None;
            self.latest_requires_http = false;
            self.owner = Some(owner);
            self.refresh_scope = None;
            self.force_http = false;
        }
        self.pending_previous_response =
            responses_previous_response_id(body).map(|id| Sha256::digest(id.as_bytes()).into());
        let previous = responses_previous_response_id(body).and_then(|id| {
            let cached = self.scope.and_then(|scope| {
                self.cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(scope, owner, id)
            });
            self.latest
                .as_ref()
                .filter(|history| {
                    history
                        .last
                        .as_ref()
                        .is_some_and(|(response_id, _)| response_id == id)
                })
                .map(|history| {
                    (
                        Arc::clone(history),
                        self.latest_requires_http
                            || cached.as_ref().is_some_and(|(_, force_http)| *force_http),
                    )
                })
                .or(cached)
        });
        self.force_http = previous.as_ref().is_some_and(|(_, force_http)| *force_http);
        self.history.requires_native_http = self.force_http;
        // A cache miss must not prevent a healthy native WS continuation.
        // Require complete history when reopening WS or falling back to HTTP.
        self.unavailable = self
            .history
            .stage_native(body, previous.as_ref().map(|(history, _)| history.as_ref()))
            .err()
            .map(|error| error.to_string());
        self.history.requires_native_http |= self.force_http;
        if self.unavailable.is_some() {
            self.history.clear_pending();
        }
    }

    pub(crate) fn restore(&mut self, owner: [u8; 32], body: &mut Value) -> Result<bool> {
        self.restore_for_http(owner, body, false)
    }

    pub(crate) fn restore_for_http(
        &mut self,
        owner: [u8; 32],
        body: &mut Value,
        allow_native_compaction: bool,
    ) -> Result<bool> {
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
        validate_native_tool_history_for_restore(input, allow_native_compaction)?;
        let input = replayable_native_history(input);
        let object = body
            .as_object_mut()
            .context("Responses 请求必须是 JSON 对象")?;
        object.insert("input".into(), Value::Array(input));
        object.remove("previous_response_id");
        Ok(true)
    }

    pub(crate) fn observe(&mut self, event: &Value) {
        if [
            event.pointer("/error/code"),
            event.pointer("/error/codey/originalCode"),
            event.pointer("/response/error/code"),
            event.pointer("/response/error/codey/originalCode"),
        ]
        .into_iter()
        .flatten()
        .any(|code| code.as_str() == Some("unsupported_persisted_item_context"))
            && let Some(id) = self.pending_previous_response
        {
            self.force_http = true;
            self.history.requires_native_http = true;
            if self.latest.as_ref().is_some_and(|history| {
                history.last.as_ref().is_some_and(|(response_id, _)| {
                    <[u8; 32]>::from(Sha256::digest(response_id.as_bytes())) == id
                })
            }) {
                self.latest_requires_http = true;
            }
            if let (Some(scope), Some(owner)) = (self.scope, self.owner) {
                self.cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .mark_requires_http(scope, owner, id);
            }
        }
        if self.history.pending_input.is_none() {
            return;
        }
        if !responses_event_is_terminal(event) {
            // A streaming upstream reports tool calls here and may still omit
            // them from the terminal output; keep them for the replay.
            self.history.record_output_item(event);
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
        let mut remembered = false;
        if complete && let Some(id) = id {
            let terminal_output = response.get("output").and_then(Value::as_array);
            if terminal_output.is_some() || self.history.has_streamed_output() {
                // Missing/oversized terminal output makes recovery unavailable;
                // it must not turn an already delivered generation into a retry.
                let output = terminal_output.map_or(&[][..], Vec::as_slice);
                if self.history.pending_input.as_ref().is_some_and(|input| {
                    input
                        .iter()
                        .any(|item| item["type"] == "compaction_trigger")
                }) {
                    // 显式压缩的 output 是完整的新窗口，不能再拼接旧历史与触发项。
                    self.history.pending_input = Some(Vec::new());
                }
                self.unavailable = self
                    .history
                    .remember(id, output)
                    .err()
                    .map(|error| error.to_string());
                remembered = self.unavailable.is_none();
            }
        }
        self.clear_pending();
        if remembered {
            let history = Arc::new(std::mem::take(&mut self.history));
            if let (Some(scope), Some(owner)) = (self.scope, self.owner) {
                self.cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(scope, owner, Arc::clone(&history));
            }
            self.latest_requires_http = history.requires_native_http;
            self.latest = Some(history);
        }
    }
}

pub(crate) fn native_item_requires_http(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        None
        | Some(
            "message"
            | "function_call"
            | "function_call_output"
            | "custom_tool_call"
            | "custom_tool_call_output",
        ) => false,
        // 完整密文与可见推理都可由原生 Responses 重放，不应仅因包含
        // encrypted_content 就把后续请求固定为 HTTP。
        Some("reasoning") => !replayable_reasoning_item(item),
        _ => true,
    }
}

fn validate_native_tool_history_for_restore(
    input: &[Value],
    allow_native_compaction: bool,
) -> Result<()> {
    if !allow_native_compaction {
        return validate_native_tool_history(input, true);
    }
    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("item_reference") => {
                anyhow::bail!("历史包含无法在协议切换时展开的引用，请重新发送完整上下文");
            }
            Some("compaction")
                if item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_none_or(|content| content.is_empty()) =>
            {
                anyhow::bail!("压缩历史缺少完整加密内容，请重新发送完整上下文");
            }
            Some("reasoning") if !replayable_reasoning_item(item) => {
                anyhow::bail!("推理历史缺少可恢复内容，请重新发送完整上下文");
            }
            _ => {}
        }
    }
    validate_native_tool_history(input, false)
}

fn tool_name_label(item: &Value) -> String {
    item.get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(|name| format!(" name={}", name.chars().take(64).collect::<String>()))
        .unwrap_or_default()
}

/// Restoring history must not be stricter than the upstream itself. Codex
/// replays reasoning items that carry only visible summaries and the upstream
/// serves them, so requiring ciphertext would reject requests the same
/// upstream already answered. Reasoning with nothing replayable is dropped
/// instead; the per-route normalization that follows decides how the
/// surviving items travel.
fn replayable_native_history(input: &[Value]) -> Vec<Value> {
    input
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) != Some("reasoning")
                || replayable_reasoning_item(item)
        })
        .cloned()
        .collect()
}

fn replayable_reasoning_item(item: &Value) -> bool {
    item.get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty())
        || reasoning_item_has_text(item)
        || summary_replay_text(item).is_some()
}

fn validate_native_tool_history(input: &[Value], restoring: bool) -> Result<()> {
    let mut calls = HashSet::new();
    for item in input {
        if codey_summary_message(item)?.is_some() {
            continue;
        }
        let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
        // compaction_trigger is a request instruction, not a history reference.
        match kind {
            "item_reference" | "compaction" if restoring => {
                anyhow::bail!("历史包含无法在协议切换时展开的引用，请重新发送完整上下文");
            }
            "function_call" | "custom_tool_call" => {
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .with_context(|| {
                        format!(
                            "工具调用缺少 call_id（{kind}{}），请重新发送完整上下文",
                            tool_name_label(item)
                        )
                    })?;
                if !calls.insert((kind, id)) {
                    anyhow::bail!(
                        "历史中工具调用 ID 重复（{kind} call_id={id}），请重新发送完整上下文"
                    );
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_kind = if kind == "function_call_output" {
                    "function_call"
                } else {
                    "custom_tool_call"
                };
                // Client-side tools (Codex app thread messaging, for example) emit
                // results that carry no call id because no model call exists for
                // them. Such an item cannot identify a dangling result, and
                // upstream accepts it, so it must not fail the whole request.
                let Some(id) = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                if !calls.remove(&(call_kind, id)) {
                    anyhow::bail!(
                        "工具结果缺少对应的完整调用历史（{kind} call_id={id}{}），请重新发送完整上下文",
                        tool_name_label(item)
                    );
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
        assert_websocket_compaction_history(crate::config::RemoteCompactionProtocol::Responses)
            .await;
    }

    #[tokio::test]
    async fn websocket_compaction_compatibility_restores_and_replaces_the_history_window() {
        assert_websocket_compaction_history(
            crate::config::RemoteCompactionProtocol::CompactEndpoint,
        )
        .await;
    }

    async fn assert_websocket_compaction_history(
        protocol: crate::config::RemoteCompactionProtocol,
    ) {
        let compat = protocol == crate::config::RemoteCompactionProtocol::CompactEndpoint;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider, model) =
            router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
        config.profiles[0].supports_websockets = true;
        config.profiles[0].supports_remote_compaction = true;
        config.profiles[0].remote_compaction_protocol = protocol;
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
            let window = json!([
                {"type":"message","role":"user","content":[{"type":"input_text","text":"retained task"}]},
                {"type":"compaction","encrypted_content":"opaque"}
            ]);
            for round in 0..2 {
                let (mut http, _) = listener.accept().await.unwrap();
                let request = read_http_request(&mut http).await.unwrap();
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.path,
                    if compat {
                        "/v1/responses/compact"
                    } else {
                        "/v1/responses"
                    }
                );
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert!(body.get("previous_response_id").is_none(), "{body}");
                let mut expected = if round == 0 {
                    json!([{"role":"user","content":"original task"}])
                } else {
                    window.clone()
                };
                if !compat {
                    expected
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"type":"compaction_trigger"}));
                }
                assert_eq!(body["input"], expected);
                let response = if compat {
                    json!({"id":"resp-compact","object":"response.compaction","output":window})
                } else {
                    completed("resp-compact", window.as_array().unwrap().clone())["response"]
                        .clone()
                };
                write_json_response(&mut http, 200, &response)
                    .await
                    .unwrap();
            }
            let (mut http, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut http).await.unwrap();
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/responses");
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body.get("previous_response_id").is_none(), "{body}");
            let mut expected = window.as_array().unwrap().clone();
            expected.push(json!({"role":"user","content":"continue after compression"}));
            assert_eq!(body["input"], json!(expected));
            write_json_response(
                &mut http,
                200,
                &completed("resp-continued", vec![])["response"],
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
        for previous in ["resp-first", "resp-compact"] {
            client
                .send(WebSocketMessage::Text(
                    json!({
                        "type":"response.create","model":model,
                        "previous_response_id":previous,
                        "input":[{"type":"compaction_trigger"}]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            assert_eq!(
                terminal(&mut client).await["response"]["id"],
                "resp-compact"
            );
        }
        client
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.create","model":model,
                    "previous_response_id":"resp-compact",
                    "input":[{"role":"user","content":"continue after compression"}]
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let continuation = terminal(&mut client).await;
        assert_eq!(continuation["type"], "response.completed", "{continuation}");
        assert_eq!(continuation["response"]["id"], "resp-continued");
        upstream.await.unwrap();
        client
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.create","model":model,
                    "previous_response_id":"resp-missing",
                    "input":[{"type":"compaction_trigger"}]
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let failure = terminal(&mut client).await;
        assert_eq!(failure["response"]["error"]["code"], "invalid_prompt");
        assert_eq!(
            failure["response"]["error"]["codey"]["errorCode"],
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
                error["response"]["error"]["codey"]["errorCode"],
                "context_not_recoverable"
            );
            assert_eq!(error["response"]["error"]["code"], "invalid_prompt");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn streamed_tool_call_survives_the_next_incremental_continuation() {
        for custom in [false, true] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let (closed_tx, closed_rx) = oneshot::channel();
            let upstream = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let first = ws.next().await.unwrap().unwrap();
                assert!(matches!(first, WebSocketMessage::Text(_)));
                // The call reaches the client only through streaming events; the
                // terminal event omits it.
                for event in [
                    json!({
                        "type":"response.output_item.added",
                        "output_index":0,
                        "item":{"id":"item-1",
                                "type":if custom {"custom_tool_call"} else {"function_call"},
                                "call_id":"call-1","name":"run","arguments":"","input":""}
                    }),
                    json!({
                        "type":"response.output_item.done",
                        "output_index":0,
                        "item":{"id":"item-1",
                                "type":if custom {"custom_tool_call"} else {"function_call"},
                                "call_id":"call-1","name":"run",
                                "arguments":"{\"cmd\":\"pwd\"}","input":"{\"cmd\":\"pwd\"}"}
                    }),
                    completed("resp-first", vec![]),
                ] {
                    ws.send(WebSocketMessage::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
                ws.close(None).await.unwrap();
                let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
                    .await
                    .unwrap();
                closed_tx.send(()).unwrap();
                // Reconnecting the upstream WebSocket fails before any request
                // is sent, so the continuation must arrive over HTTP expanded.
                let (mut rejected, _) = listener.accept().await.unwrap();
                assert_eq!(
                    read_http_request(&mut rejected).await.unwrap().method,
                    "GET"
                );
                rejected
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
                drop(rejected);
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_http_request(&mut socket).await.unwrap();
                assert_eq!(request.method, "POST");
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert!(body.get("previous_response_id").is_none(), "{body}");
                assert_eq!(
                    body["input"],
                    json!([
                        {"role":"user","content":"original task"},
                        {"id":"item-1",
                         "type":if custom {"custom_tool_call"} else {"function_call"},
                         "call_id":"call-1","name":"run",
                         "arguments":"{\"cmd\":\"pwd\"}","input":"{\"cmd\":\"pwd\"}"},
                        result(custom, "call-1"),
                    ]),
                    "{body}"
                );
                write_json_response(
                    &mut socket,
                    200,
                    &completed("resp-second", vec![])["response"],
                )
                .await
                .unwrap();
            });
            let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
            config.profiles[0].supports_websockets = true;
            let router = LocalRouter::start(&config).await.unwrap();
            let mut client = connect_router_websocket_with_headers(
                &router.endpoint(),
                &[("session-id", "streamed-call-session")],
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
                &[("session-id", "streamed-call-session")],
            )
            .await;
            client
                .send(WebSocketMessage::Text(
                    json!({
                        "type":"response.create","model":model,
                        "previous_response_id":"resp-first",
                        "input":[result(custom, "call-1")]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            assert_eq!(terminal(&mut client).await["response"]["id"], "resp-second");
            upstream.await.unwrap();
            client.close(None).await.unwrap();
            router.stop().await.unwrap();
        }
    }

    #[test]
    fn summary_only_reasoning_history_is_replayed_instead_of_rejected() {
        // Codex replays reasoning that carries only visible summaries, and the
        // upstream serves those requests. Restoring must not be stricter.
        for reasoning in [
            json!({"type":"reasoning","id":"rs-summary","summary":[{"type":"summary_text","text":"plan"}],"content":[]}),
            json!({"type":"reasoning","id":"rs-plain","summary":[],"content":[{"type":"reasoning_text","text":"think"}]}),
            json!({"type":"reasoning","id":"rs-cipher","encrypted_content":"opaque","summary":[]}),
        ] {
            let mut history = NativeResponsesHistory::default();
            let key = [1; 32];
            let mut context = vec![json!({"role":"user","content":"task"}), reasoning.clone()];
            context.extend([call(false, "call-1"), result(false, "call-1")]);
            history.prepare(key, &mut json!({"input": context}));
            history.observe(&completed("resp-known", vec![call(false, "call-2")]));
            let mut next = json!({
                "previous_response_id":"resp-known",
                "input":[result(false, "call-2")]
            });
            history.prepare(key, &mut next);
            history.restore(key, &mut next).unwrap();
            let input = next["input"].as_array().unwrap();
            assert_eq!(input[1], reasoning, "{next}");
            assert_eq!(input.len(), 6, "{next}");
        }
    }

    #[test]
    fn reasoning_with_nothing_to_replay_is_dropped() {
        let mut history = NativeResponsesHistory::default();
        let key = [1; 32];
        history.prepare(
            key,
            &mut json!({"input":[
                {"role":"user","content":"task"},
                {"type":"reasoning","id":"rs-hollow","summary":[],"content":[]},
                call(false, "call-1"),
                result(false, "call-1")
            ]}),
        );
        history.observe(&completed("resp-known", vec![call(false, "call-2")]));
        let mut next = json!({
            "previous_response_id":"resp-known",
            "input":[result(false, "call-2")]
        });
        history.prepare(key, &mut next);
        history.restore(key, &mut next).unwrap();
        assert_eq!(
            next["input"],
            json!([
                {"role":"user","content":"task"},
                call(false, "call-1"),
                result(false, "call-1"),
                call(false, "call-2"),
                result(false, "call-2")
            ]),
            "{next}"
        );
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
            let error = history.restore(key, &mut orphan).unwrap_err().to_string();
            assert!(error.contains("工具结果缺少对应的完整调用历史"), "{error}");
            assert!(error.contains("call_id=missing-call"), "{error}");
        }
    }

    #[test]
    fn streamed_reasoning_uses_completed_content_for_http_selection() {
        let visible = json!({
            "id":"reasoning","type":"reasoning",
            "summary":[{"type":"summary_text","text":"plan"}]
        });
        for (done, terminal) in [(true, false), (false, true), (true, true)] {
            let owner = [1; 32];
            let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
            let headers = vec![("session-id".into(), "visible-reasoning".into())];
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
            history.prepare(owner, &mut json!({"input":"task"}));
            history.observe(&json!({
                "type":"response.output_item.added","output_index":0,
                "item":{"id":"reasoning","type":"reasoning","summary":[]}
            }));
            if done {
                history.observe(&json!({
                    "type":"response.output_item.done","output_index":0,"item":visible
                }));
            }
            history.observe(&completed(
                "resp-visible",
                if terminal {
                    vec![visible.clone()]
                } else {
                    vec![]
                },
            ));
            let mut restored = NativeResponsesHistory::with_cache(cache, &headers);
            let mut next = json!({"previous_response_id":"resp-visible","input":"next"});
            restored.prepare(owner, &mut next);
            assert!(!restored.requires_http(), "{done} {terminal}");
            assert!(restored.restore(owner, &mut next).unwrap());
            assert_eq!(next["input"][1], visible);
            assert_eq!(next["input"].as_array().unwrap().len(), 3);
        }
    }

    #[test]
    fn encrypted_reasoning_does_not_force_later_continuations_to_http() {
        let owner = [1; 32];
        let mut history = NativeResponsesHistory::default();
        history.prepare(owner, &mut json!({"input":"task"}));
        history.observe(&completed(
            "resp-encrypted",
            vec![json!({"type":"reasoning","encrypted_content":"ciphertext"})],
        ));
        let mut next = json!({"previous_response_id":"resp-encrypted","input":"next"});
        history.prepare(owner, &mut next);
        assert!(!history.requires_http());
        assert!(history.restore(owner, &mut next).unwrap());
        assert_eq!(next["input"][1]["encrypted_content"], "ciphertext");
        history.observe(&completed("resp-next", vec![]));
        history.prepare(
            owner,
            &mut json!({"previous_response_id":"resp-next","input":"later"}),
        );
        assert!(!history.requires_http());
    }

    #[test]
    fn completed_visible_reasoning_preserves_opaque_or_rejected_history() {
        let owner = [1; 32];
        for previous in [
            Some(json!({"id":"search","type":"web_search_call","status":"completed"})),
            None,
        ] {
            let mut history = NativeResponsesHistory::default();
            history.prepare(owner, &mut json!({"input":"task"}));
            history.observe(&completed(
                "resp-opaque",
                previous.clone().into_iter().collect(),
            ));
            let mut body = json!({"previous_response_id":"resp-opaque","input":"next"});
            history.prepare(owner, &mut body);
            if previous.is_none() {
                history.observe(&json!({"type":"error","error":{
                    "code":"unsupported_persisted_item_context"
                }}));
                history.prepare(owner, &mut body);
            }
            history.observe(&json!({
                "type":"response.output_item.added","output_index":0,
                "item":{"id":"visible","type":"reasoning","summary":[]}
            }));
            history.observe(&completed(
                "resp-next",
                vec![json!({
                    "id":"visible","type":"reasoning",
                    "summary":[{"type":"summary_text","text":"plan"}]
                })],
            ));
            history.prepare(
                owner,
                &mut json!({"previous_response_id":"resp-next","input":"later"}),
            );
            assert!(history.requires_http(), "{previous:?}");
        }
    }

    #[test]
    fn streamed_history_accepts_the_item_limit_and_repeated_updates() {
        let owner = [1; 32];
        let mut history = NativeResponsesHistory::default();
        history.prepare(owner, &mut json!({"input":"task"}));
        for index in 0..512 {
            let item = json!({"id":format!("message-{index}"),"type":"message","role":"assistant","content":[]});
            for kind in ["response.output_item.added", "response.output_item.done"] {
                history.observe(&json!({"type":kind,"output_index":index,"item":item}));
            }
        }
        history.observe(&completed("resp-boundary", vec![]));
        assert!(history.unavailable.is_none());
        let mut next = json!({"previous_response_id":"resp-boundary","input":"next"});
        history.prepare(owner, &mut next);
        assert!(history.restore(owner, &mut next).unwrap());
        assert_eq!(next["input"].as_array().unwrap().len(), 514);
    }

    #[test]
    fn streamed_history_overflow_never_publishes_a_partial_snapshot() {
        for terminal_count in [0, 512, 513] {
            let owner = [1; 32];
            let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
            let headers = vec![("session-id".into(), "overflow-session".into())];
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
            history.prepare(owner, &mut json!({"input":"task"}));
            history.observe(&completed("resp-old", vec![]));
            history.prepare(
                owner,
                &mut json!({"previous_response_id":"resp-old","input":"next"}),
            );
            let output: Vec<Value> = (0..513)
                .map(|index| json!({"id":format!("message-{index}"),"type":"message","role":"assistant","content":[]}))
                .collect();
            for (index, item) in output.iter().enumerate() {
                history.observe(&json!({
                    "type":"response.output_item.done","output_index":index,"item":item
                }));
            }
            history.observe(&completed(
                "resp-overflow",
                output[..terminal_count].to_vec(),
            ));
            assert!(
                history
                    .unavailable
                    .as_ref()
                    .unwrap()
                    .contains("流式输出条目超过")
            );
            assert_eq!(
                history.latest.as_ref().unwrap().last.as_ref().unwrap().0,
                "resp-old"
            );
            assert_eq!(cache.lock().unwrap().entries.len(), 1);
            let next = json!({"previous_response_id":"resp-overflow","input":"later"});
            let mut same_connection = next.clone();
            history.prepare(owner, &mut same_connection);
            assert!(
                history
                    .restore_for_http(owner, &mut same_connection, true)
                    .is_err()
            );
            assert_eq!(same_connection, next);
            let mut reconnected = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
            let mut shared = next.clone();
            reconnected.prepare(owner, &mut shared);
            assert!(
                reconnected
                    .restore_for_http(owner, &mut shared, true)
                    .is_err()
            );
            assert_eq!(shared, next);
            let mut old = json!({"previous_response_id":"resp-old","input":"branch"});
            reconnected.prepare(owner, &mut old);
            assert!(reconnected.restore(owner, &mut old).unwrap());
            assert_eq!(old["input"].as_array().unwrap().len(), 2);
            history.prepare(owner, &mut json!({"input":"independent"}));
            history.observe(&json!({
                "type":"response.output_item.done","output_index":0,
                "item":{"id":"normal","type":"message","role":"assistant","content":[]}
            }));
            history.observe(&completed("resp-normal", vec![]));
            let mut independent = json!({"previous_response_id":"resp-normal","input":"next"});
            history.prepare(owner, &mut independent);
            assert!(history.restore(owner, &mut independent).unwrap());
            assert_eq!(independent["input"].as_array().unwrap().len(), 3);
            let mut reconnected = NativeResponsesHistory::with_cache(cache, &headers);
            let mut shared = json!({"previous_response_id":"resp-normal","input":"next"});
            reconnected.prepare(owner, &mut shared);
            assert!(reconnected.restore(owner, &mut shared).unwrap());
        }
    }

    #[test]
    fn streamed_history_resets_overflow_when_the_pending_turn_is_abandoned() {
        for clear_pending in [false, true] {
            let owner = [1; 32];
            let mut history = NativeResponsesHistory::default();
            history.prepare(owner, &mut json!({"input":"abandoned"}));
            for index in 0..513 {
                history.observe(&json!({
                    "type":"response.output_item.done","output_index":index,
                    "item":{"id":format!("abandoned-{index}"),"type":"message","role":"assistant","content":[]}
                }));
            }
            if clear_pending {
                history.clear_pending();
            }
            history.prepare(owner, &mut json!({"input":"independent"}));
            history.observe(&completed("resp-normal", vec![]));
            assert!(history.unavailable.is_none());
            let mut next = json!({"previous_response_id":"resp-normal","input":"next"});
            history.prepare(owner, &mut next);
            assert!(history.restore(owner, &mut next).unwrap());
            assert_eq!(
                next["input"],
                json!([
                    {"role":"user","content":"independent"},
                    {"role":"user","content":"next"}
                ])
            );
        }
    }

    #[test]
    fn streamed_tool_call_missing_from_terminal_output_still_pairs_its_result() {
        for custom in [false, true] {
            let payload_field = if custom { "input" } else { "arguments" };
            let mut history = NativeResponsesHistory::default();
            let key = [1; 32];
            history.prepare(key, &mut json!({"input":"task"}));
            // The upstream reports the call only through streaming events and
            // then completes with an output that omits it.
            history.observe(&json!({
                "type":"response.output_item.added",
                "output_index":0,
                "item":{"id":"item-1","type":if custom {"custom_tool_call"} else {"function_call"},
                        "call_id":"call-1","name":"run", "arguments":"", "input":""}
            }));
            history.observe(&json!({
                "type":"response.output_item.done",
                "output_index":0,
                "item":{"id":"item-1","type":if custom {"custom_tool_call"} else {"function_call"},
                        "call_id":"call-1","name":"run", "arguments":"{\"cmd\":\"pwd\"}",
                        "input":"{\"cmd\":\"pwd\"}"}
            }));
            history.observe(&json!({
                "type":"response.completed",
                "response":{"id":"resp-streamed","status":"completed","output":[]}
            }));
            let mut next = json!({
                "previous_response_id":"resp-streamed",
                "input":[result(custom, "call-1")]
            });
            history.prepare(key, &mut next);
            history.restore(key, &mut next).unwrap();
            let input = next["input"].as_array().unwrap();
            assert_eq!(input.len(), 3, "{next}");
            assert_eq!(input[1]["call_id"], "call-1");
            assert_eq!(input[1][payload_field], "{\"cmd\":\"pwd\"}");
        }
    }

    #[test]
    fn streamed_call_already_in_the_terminal_output_is_not_duplicated() {
        let mut history = NativeResponsesHistory::default();
        let key = [1; 32];
        history.prepare(key, &mut json!({"input":"task"}));
        history.observe(&json!({
            "type":"response.output_item.done",
            "output_index":0,
            "item":{"id":"item-streamed","type":"function_call","call_id":"call-1","name":"run","arguments":"{}"}
        }));
        // The terminal event carries the same call under a renumbered item id.
        history.observe(&completed(
            "resp-both",
            vec![json!({
                "id":"item-terminal","type":"function_call","call_id":"call-1","name":"run","arguments":"{}"
            })],
        ));
        let mut next =
            json!({"previous_response_id":"resp-both","input":[result(false, "call-1")]});
        history.prepare(key, &mut next);
        history.restore(key, &mut next).unwrap();
        assert_eq!(
            next["input"],
            json!([
                {"role":"user","content":"task"},
                {"id":"item-terminal","type":"function_call","call_id":"call-1","name":"run","arguments":"{}"},
                result(false, "call-1")
            ])
        );
    }

    #[test]
    fn streamed_anonymous_output_preserves_distinct_positions_without_duplicates() {
        let message = json!({
            "type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"same message"}]
        });
        for terminal_count in 0..=2 {
            let mut history = NativeResponsesHistory::default();
            let key = [1; 32];
            history.prepare(key, &mut json!({"input":"task"}));
            for index in 0..2 {
                history.observe(&json!({
                    "type":"response.output_item.done","output_index":index,
                    "item":message
                }));
            }
            history.observe(&completed(
                "resp-anonymous",
                vec![message.clone(); terminal_count],
            ));
            let mut next = json!({"previous_response_id":"resp-anonymous","input":"next"});
            history.prepare(key, &mut next);
            history.restore(key, &mut next).unwrap();
            assert_eq!(
                next["input"],
                json!([
                    {"role":"user","content":"task"},
                    message, message,
                    {"role":"user","content":"next"}
                ]),
                "terminal_count={terminal_count}"
            );
        }
    }

    #[test]
    fn streamed_call_without_payload_is_never_replayed() {
        let mut history = NativeResponsesHistory::default();
        let key = [1; 32];
        history.prepare(key, &mut json!({"input":"task"}));
        // No arguments event arrives, so the call cannot be reconstructed and
        // must not be replayed half-formed.
        history.observe(&json!({
            "type":"response.output_item.added",
            "output_index":0,
            "item":{"id":"item-1","type":"function_call","call_id":"call-1","name":"run","arguments":""}
        }));
        history.observe(&json!({
            "type":"response.completed",
            "response":{"id":"resp-hollow","status":"completed","output":[]}
        }));
        let mut next =
            json!({"previous_response_id":"resp-hollow","input":[result(false, "call-1")]});
        history.prepare(key, &mut next);
        let error = history.restore(key, &mut next).unwrap_err().to_string();
        assert!(error.contains("工具结果缺少对应的完整调用历史"), "{error}");
    }

    #[test]
    fn native_history_tolerates_results_without_call_id() {
        for custom in [false, true] {
            let mut history = NativeResponsesHistory::default();
            let key = [1; 32];
            // Client-side tools emit results that carry no call id at all.
            let mut sideless = result(custom, "unused");
            assert!(
                sideless
                    .as_object_mut()
                    .unwrap()
                    .remove("call_id")
                    .is_some()
            );
            sideless["name"] = json!("send_message_to_thread");
            // A complete array request must pass, matching the HTTP/SSE path.
            let mut complete = json!({
                "input":[sideless.clone(), call(custom, "call-1"), result(custom, "call-1")]
            });
            history.prepare(key, &mut complete);
            assert!(history.restore(key, &mut complete).is_ok());
            assert_eq!(complete["input"].as_array().unwrap().len(), 3);
            // The same holds when history is restored onto a native continuation.
            history.prepare(key, &mut json!({"input":"task"}));
            history.observe(&completed("resp-known", vec![call(custom, "call-2")]));
            let mut next = json!({
                "previous_response_id":"resp-known",
                "input":[sideless, result(custom, "call-2")]
            });
            history.prepare(key, &mut next);
            assert!(history.restore(key, &mut next).unwrap());
            assert_eq!(next["input"].as_array().unwrap().len(), 4);
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
        for index in 0..=NATIVE_HISTORY_CACHE_ENTRIES {
            history.prepare([1; 32], &mut json!({"input":"task"}));
            history.observe(&completed(&format!("resp-{index}"), vec![]));
        }
        let scope = history.scope.unwrap();
        assert_eq!(
            cache.lock().unwrap().entries.len(),
            NATIVE_HISTORY_CACHE_ENTRIES
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

    #[test]
    fn official_token_refresh_keeps_only_the_current_connection_history() {
        let (config, provider, model) = router_config("http://127.0.0.1:9/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        let original = UpstreamWebSocketAuthIdentity {
            authorization: Some([1; 32]),
            account_id: Some([2; 32]),
        };
        let refreshed = UpstreamWebSocketAuthIdentity {
            authorization: Some([3; 32]),
            ..original
        };
        for (official, account_id, same_model, allowed) in [
            (true, original.account_id, true, true),
            (false, original.account_id, true, false),
            (true, Some([4; 32]), true, false),
            (true, None, true, false),
            (true, original.account_id, false, false),
        ] {
            let mut route = snapshot.routes[&provider].as_ref().clone();
            route.official_account = official;
            let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
            let headers = vec![("session-id".into(), "refresh-session".into())];
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
            history.prepare_for_route(&route, original, &mut json!({"model":model,"input":"task"}));
            history.observe(&completed("resp-refresh", vec![]));
            history.prepare_for_route(
                &route,
                original,
                &mut json!({"model":model,"previous_response_id":"resp-refresh","input":"retry"}),
            );
            history.observe(
                &json!({"type":"error","error":{"code":"unsupported_persisted_item_context"}}),
            );
            let next_auth = UpstreamWebSocketAuthIdentity {
                account_id,
                ..refreshed
            };
            let next_body = json!({
                "model":if same_model { model.as_str() } else { "other-model" },
                "previous_response_id":"resp-refresh","input":"next"
            });
            let mut separate = NativeResponsesHistory::with_cache(cache, &headers);
            let mut separate_body = next_body.clone();
            let owner = separate.prepare_for_route(&route, next_auth, &mut separate_body);
            assert!(
                separate
                    .restore_for_http(owner, &mut separate_body, true)
                    .is_err()
            );
            let mut next = next_body;
            let owner = history.prepare_for_route(&route, next_auth, &mut next);
            assert_eq!(history.requires_http(), allowed);
            assert_eq!(
                history.restore_for_http(owner, &mut next, true).is_ok(),
                allowed
            );
            if allowed {
                assert_eq!(
                    next["input"],
                    json!([
                        {"role":"user","content":"task"},
                        {"role":"user","content":"next"}
                    ])
                );
            } else {
                assert_eq!(next["previous_response_id"], "resp-refresh");
            }
        }
    }

    #[test]
    fn native_http_history_preserves_hosted_and_opaque_items_from_streams_and_cache() {
        for item in [
            json!({"type":"web_search_call","id":"search","status":"completed","action":{"type":"search","query":"test"}}),
            json!({"type":"file_search_call","id":"file","status":"completed","queries":["test"]}),
            json!({"type":"reasoning","id":"reasoning","encrypted_content":"ciphertext","summary":[]}),
            json!({"type":"compaction","id":"compact","encrypted_content":"compacted-state"}),
        ] {
            for streamed in [false, true] {
                let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
                let headers = vec![("session-id".into(), "opaque-session".into())];
                let owner = [1; 32];
                let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
                history.prepare(owner, &mut json!({"input":"task"}));
                let output = if streamed {
                    history.observe(
                        &json!({"type":"response.output_item.done","output_index":0,"item":item}),
                    );
                    vec![]
                } else {
                    vec![item.clone()]
                };
                history.observe(&completed("resp-opaque", output));
                let mut restored = NativeResponsesHistory::with_cache(cache, &headers);
                let mut next = json!({"previous_response_id":"resp-opaque","input":"next"});
                restored.prepare(owner, &mut next);
                assert_eq!(
                    restored.requires_http(),
                    item["type"] != "reasoning",
                    "{item}"
                );
                assert!(restored.restore_for_http(owner, &mut next, true).unwrap());
                assert_eq!(next["input"][1], item);
                assert!(next.get("previous_response_id").is_none());
            }
        }
    }

    #[test]
    fn native_http_history_rejects_unrecoverable_state_without_dropping_it() {
        for item in [
            json!({"type":"item_reference","id":"unknown"}),
            json!({"type":"compaction","id":"missing-payload"}),
            json!({"type":"reasoning","id":"hidden","summary":[]}),
        ] {
            let owner = [1; 32];
            let mut history = NativeResponsesHistory::default();
            history.prepare(owner, &mut json!({"input":"task"}));
            history
                .observe(&json!({"type":"response.output_item.done","output_index":0,"item":item}));
            history.observe(&completed("resp-hidden", vec![]));
            let mut next = json!({"previous_response_id":"resp-hidden","input":"next"});
            history.prepare(owner, &mut next);
            assert!(history.requires_http());
            let original = next.clone();
            assert!(
                history.restore_for_http(owner, &mut next, true).is_err(),
                "{item}"
            );
            assert_eq!(next, original);
        }
    }

    #[test]
    fn native_http_rejection_hint_survives_failure_normalization() {
        let mut history = NativeResponsesHistory::default();
        history.prepare([1; 32], &mut json!({"input":"task"}));
        history.observe(&completed("resp-original", vec![]));
        let mut body = json!({"previous_response_id":"resp-original","input":"next"});
        history.prepare([1; 32], &mut body);
        assert!(!history.requires_http());
        let mut error = json!({"type":"error","error":{
            "type":"invalid_request_error","code":"unsupported_persisted_item_context",
            "message":"Rustponses cannot replay persisted state"
        }});
        normalize_response_failure(&mut error, None);
        history.observe(&error);
        history.prepare([1; 32], &mut body);
        assert!(history.requires_http());
        assert!(history.restore_for_http([1; 32], &mut body, true).unwrap());
        history.prepare([2; 32], &mut json!({"input":"other account"}));
        assert!(!history.requires_http());
    }

    #[test]
    fn native_http_rejection_survives_reconnection_without_affecting_other_snapshots() {
        let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
        let headers = vec![("session-id".into(), "rejected-session".into())];
        let other_headers = vec![("session-id".into(), "other-session".into())];
        for (scope_headers, owner, response_id) in [
            (&headers, [1; 32], "resp-rejected"),
            (&headers, [1; 32], "resp-other"),
            (&headers, [2; 32], "resp-rejected"),
            (&other_headers, [1; 32], "resp-rejected"),
        ] {
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), scope_headers);
            history.prepare(owner, &mut json!({"input":"task"}));
            history.observe(&completed(response_id, vec![]));
        }
        let mut rejected = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
        rejected.prepare(
            [1; 32],
            &mut json!({"previous_response_id":"resp-rejected","input":"next"}),
        );
        let mut error = json!({"type":"error","error":{
            "type":"invalid_request_error","code":"unsupported_persisted_item_context",
            "message":"Rustponses cannot replay persisted state"
        }});
        normalize_response_failure(&mut error, None);
        rejected.observe(&error);
        drop(rejected);
        for (scope_headers, owner, response_id, requires_http) in [
            (&headers, [1; 32], "resp-rejected", true),
            (&headers, [1; 32], "resp-other", false),
            (&headers, [2; 32], "resp-rejected", false),
            (&other_headers, [1; 32], "resp-rejected", false),
        ] {
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), scope_headers);
            let mut next = json!({"previous_response_id":response_id,"input":"next"});
            history.prepare(owner, &mut next);
            assert_eq!(history.requires_http(), requires_http);
            assert!(history.restore_for_http(owner, &mut next, true).unwrap());
            assert_eq!(next["input"].as_array().unwrap().len(), 2);
            if requires_http {
                history.observe(&completed("resp-http-success", vec![]));
            }
        }
        let mut inherited = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
        let mut next = json!({"previous_response_id":"resp-http-success","input":"later"});
        inherited.prepare([1; 32], &mut next);
        assert!(inherited.requires_http());
        assert!(
            inherited
                .restore_for_http([1; 32], &mut next, true)
                .unwrap()
        );
        assert_eq!(next["input"].as_array().unwrap().len(), 3);
        drop(inherited);
        cache
            .lock()
            .unwrap()
            .prune(Instant::now() + NATIVE_HISTORY_CACHE_TTL);
        let mut expired = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
        let mut next = json!({"previous_response_id":"resp-rejected","input":"retry"});
        expired.prepare([1; 32], &mut next);
        assert!(!expired.requires_http());
        assert!(expired.restore_for_http([1; 32], &mut next, true).is_err());
        assert!(cache.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn native_http_rejection_only_marks_the_pending_continuation() {
        let mut history = NativeResponsesHistory::default();
        let owner = [1; 32];
        let error = json!({"type":"error","error":{"code":"unsupported_persisted_item_context"}});
        history.prepare(owner, &mut json!({"input":"task"}));
        history.observe(&completed("resp-original", vec![]));
        history.observe(&error);
        let mut next = json!({"previous_response_id":"resp-original","input":"next"});
        history.prepare(owner, &mut next);
        assert!(!history.requires_http());
        history.clear_pending();
        history.observe(&error);
        history.prepare(owner, &mut next);
        assert!(!history.requires_http());
        history.observe(&error);
        history.prepare(owner, &mut next);
        assert!(history.requires_http());
        history.prepare(
            owner,
            &mut json!({"previous_response_id":"resp-missing","input":"unrelated continuation"}),
        );
        assert!(!history.requires_http());
        history.prepare(owner, &mut json!({"input":"independent task"}));
        assert!(!history.requires_http());
        history.prepare(owner, &mut next);
        assert!(history.requires_http());
    }
}
