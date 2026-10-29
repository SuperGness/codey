use super::*;

#[derive(Debug)]
pub(crate) struct MissingResponseHistory;

impl std::fmt::Display for MissingResponseHistory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("会话历史已失效，请重新发送完整输入")
    }
}

impl std::error::Error for MissingResponseHistory {}

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
    continuation_rejected: bool,
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
    ) -> Result<Option<(Arc<AdaptedResponsesHistory>, bool)>> {
        self.prune(Instant::now());
        // Linear scan of at most NATIVE_HISTORY_CACHE_ENTRIES snapshots; use a
        // map if this bound grows.
        let mut other_owner = false;
        for entry in self.entries.iter().rev().filter(|entry| {
            entry.scope == scope
                && entry
                    .history
                    .last
                    .as_ref()
                    .is_some_and(|(response_id, _)| response_id == id)
        }) {
            if entry.owner == owner {
                return Ok(Some((
                    Arc::clone(&entry.history),
                    entry.continuation_rejected,
                )));
            }
            other_owner = true;
        }
        if other_owner {
            anyhow::bail!("上游身份已变化，无法安全恢复续接历史；请重新发送完整上下文");
        }
        Ok(None)
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
                entry.continuation_rejected = true;
            }
        }
    }

    fn insert(
        &mut self,
        scope: [u8; 32],
        owner: [u8; 32],
        history: Arc<AdaptedResponsesHistory>,
        continuation_rejected: bool,
    ) {
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
            continuation_rejected,
            history,
        });
    }
}

#[derive(Default)]
pub(crate) struct NativeResponsesHistory {
    owner: Option<[u8; 32]>,
    refresh_scope: Option<[u8; 32]>,
    continuation_rejected: bool,
    pending_previous_response: Option<[u8; 32]>,
    scope: Option<[u8; 32]>,
    cache: Arc<Mutex<NativeHistoryCache>>,
    latest: Option<Arc<AdaptedResponsesHistory>>,
    latest_continuation_rejected: bool,
    history: AdaptedResponsesHistory,
    unavailable: Option<anyhow::Error>,
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
        if self.owner == Some(owner) {
            self.refresh_scope = refresh_scope;
        }
        owner
    }

    pub(crate) fn requires_http(&self) -> bool {
        self.continuation_rejected || self.history.requires_native_http
    }

    pub(crate) fn requires_http_for_websocket(
        &self,
        body: &Value,
        continues_on_current_socket: bool,
    ) -> bool {
        if !continues_on_current_socket || self.pending_previous_response.is_none() {
            return self.requires_http();
        }
        // 同一连接已保存旧历史，仅检查新增输入；上游明确拒绝的续接仍需 HTTP。
        self.continuation_rejected
            || match body.get("input") {
                Some(Value::Array(input)) => input.iter().any(native_item_requires_http),
                Some(item @ Value::Object(_)) => native_item_requires_http(item),
                _ => false,
            }
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
        self.pending_previous_response =
            responses_previous_response_id(body).map(|id| Sha256::digest(id.as_bytes()).into());
        if self
            .owner
            .is_some_and(|previous_owner| previous_owner != owner)
            && responses_previous_response_id(body).is_some_and(|id| {
                self.latest.as_ref().is_some_and(|history| {
                    history
                        .last
                        .as_ref()
                        .is_some_and(|(response_id, _)| response_id == id)
                })
            })
        {
            // 保留原身份，HTTP 回退再次 prepare 时仍须拒绝这条续接。
            self.history.clear_pending();
            self.history.requires_native_http = false;
            self.continuation_rejected = false;
            self.unavailable = Some(anyhow::anyhow!(
                "上游身份已变化，无法安全恢复续接历史；请重新发送完整上下文"
            ));
            return;
        }
        if self.owner != Some(owner) {
            self.history = AdaptedResponsesHistory::default();
            self.latest = None;
            self.latest_continuation_rejected = false;
            self.owner = Some(owner);
            self.refresh_scope = None;
            self.continuation_rejected = false;
        }
        let previous = responses_previous_response_id(body)
            .map(|id| {
                let latest = self.latest.as_ref().filter(|history| {
                    history
                        .last
                        .as_ref()
                        .is_some_and(|(response_id, _)| response_id == id)
                });
                let cached = self.scope.map_or(Ok(None), |scope| {
                    self.cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(scope, owner, id)
                });
                let cached = match cached {
                    Ok(cached) => cached,
                    // 同账号令牌刷新只允许使用当前连接已确认归属的快照。
                    Err(_) if latest.is_some() => None,
                    Err(error) => return Err(error),
                };
                Ok(latest
                    .map(|history| {
                        (
                            Arc::clone(history),
                            self.latest_continuation_rejected
                                || cached.as_ref().is_some_and(|(_, continuation_rejected)| {
                                    *continuation_rejected
                                }),
                        )
                    })
                    .or(cached))
            })
            .transpose();
        let previous = match previous {
            Ok(previous) => previous.flatten(),
            Err(error) => {
                self.history.clear_pending();
                self.history.requires_native_http = false;
                self.continuation_rejected = false;
                self.unavailable = Some(error);
                return;
            }
        };
        self.continuation_rejected = previous
            .as_ref()
            .is_some_and(|(_, continuation_rejected)| *continuation_rejected);
        self.history.requires_native_http = false;
        // A cache miss must not prevent a healthy native WS continuation.
        // Require complete history when reopening WS or falling back to HTTP.
        self.unavailable = self
            .history
            .stage_native(body, previous.as_ref().map(|(history, _)| history.as_ref()))
            .err();
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
        let input = self.validated_http_restore_input(owner, allow_native_compaction)?;
        let input = replayable_native_history(input);
        let object = body
            .as_object_mut()
            .context("Responses 请求必须是 JSON 对象")?;
        object.insert("input".into(), Value::Array(input));
        object.remove("previous_response_id");
        Ok(true)
    }

    fn validated_http_restore_input(
        &self,
        owner: [u8; 32],
        allow_native_compaction: bool,
    ) -> Result<&[Value]> {
        if self.owner != Some(owner) {
            anyhow::bail!("上游身份已变化，无法安全恢复续接历史；请重新发送完整上下文");
        }
        if let Some(reason) = &self.unavailable {
            if reason.is::<MissingResponseHistory>() {
                return Err(MissingResponseHistory.into());
            }
            anyhow::bail!("无法恢复原生续接历史：{reason}；请重新发送完整上下文");
        }
        let input = self
            .history
            .pending_input
            .as_ref()
            .context("缺少完整续接历史，请重新发送完整上下文")?;
        validate_native_tool_history_for_restore(input, allow_native_compaction)?;
        Ok(input)
    }

    pub(crate) fn can_recover_rejected_continuation(
        &self,
        owner: [u8; 32],
        allow_native_compaction: bool,
    ) -> bool {
        self.pending_previous_response.is_some()
            && self
                .validated_http_restore_input(owner, allow_native_compaction)
                .is_ok_and(|input| {
                    // 同轮恢复必须保留全部上下文，不能靠丢弃空推理绕过拒绝。
                    input
                        .iter()
                        .all(|item| item["type"] != "reasoning" || replayable_reasoning_item(item))
                })
    }

    pub(crate) fn mark_continuation_rejected(&mut self) {
        let Some(id) = self.pending_previous_response else {
            return;
        };
        self.continuation_rejected = true;
        if self.latest.as_ref().is_some_and(|history| {
            history.last.as_ref().is_some_and(|(response_id, _)| {
                <[u8; 32]>::from(Sha256::digest(response_id.as_bytes())) == id
            })
        }) {
            self.latest_continuation_rejected = true;
        }
        if let (Some(scope), Some(owner)) = (self.scope, self.owner) {
            self.cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .mark_requires_http(scope, owner, id);
        }
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
        {
            self.mark_continuation_rejected();
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
        let compacting = self.history.pending_input.as_ref().is_some_and(|input| {
            input
                .iter()
                .any(|item| item["type"] == "compaction_trigger")
        });
        let status = response.get("status").and_then(Value::as_str);
        // 被中断或截断的响应仍由上游保存，Codex 会直接以它的 id 续接；
        // 未完成的压缩不是完整窗口，必须保留上一份历史。
        let complete = match event.get("type").and_then(Value::as_str) {
            Some("response.completed") => status.is_none_or(|status| {
                status == "completed" || (!compacting && status == "incomplete")
            }),
            Some("response.incomplete") => !compacting,
            _ => false,
        };
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
                if compacting {
                    // 显式压缩的 output 是完整的新窗口，不能再拼接旧历史与触发项。
                    self.history.pending_input = Some(Vec::new());
                }
                self.unavailable = self.history.remember(id, output).err();
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
                    .insert(
                        scope,
                        owner,
                        Arc::clone(&history),
                        self.continuation_rejected,
                    );
            }
            self.latest_continuation_rejected = self.continuation_rejected;
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
        // 这些客户端条目自带正文或工具定义，不依赖上游持久化的历史状态。
        Some("additional_tools") => {
            item.get("role").and_then(Value::as_str) != Some("developer")
                || !replayable_native_tool_definitions(item)
        }
        Some("agent_message") => !replayable_native_agent_message(item),
        Some(kind @ ("tool_search_call" | "tool_search_output")) => {
            item.get("execution").and_then(Value::as_str) != Some("client")
                || !nonempty_native_history_string(item.get("call_id"))
                || if kind == "tool_search_call" {
                    !item.get("arguments").is_some_and(Value::is_object)
                } else {
                    !replayable_native_tool_definitions(item)
                }
        }
        _ => true,
    }
}

fn nonempty_native_history_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

fn replayable_native_tool_definitions(item: &Value) -> bool {
    item.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .all(|tool| nonempty_native_history_string(tool.get("type")))
        })
}

fn replayable_native_agent_message(item: &Value) -> bool {
    fn complete_part(part: &Value) -> bool {
        match part.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "text") => {
                nonempty_native_history_string(part.get("text"))
            }
            // 原生标准化保留令牌形态的密文，并把其余非空正文转成文本。
            Some("encrypted_content") => {
                nonempty_native_history_string(part.get("encrypted_content"))
            }
            _ => false,
        }
    }
    match item.get("content") {
        None | Some(Value::Null) => nonempty_native_history_string(item.get("message")),
        Some(content @ Value::String(_)) => nonempty_native_history_string(Some(content)),
        Some(Value::Array(parts)) => !parts.is_empty() && parts.iter().all(complete_part),
        Some(part @ Value::Object(_)) => complete_part(part),
        _ => false,
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
            "function_call" | "custom_tool_call" | "tool_search_call"
                if kind != "tool_search_call" || item["execution"] == "client" =>
            {
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
            "function_call_output" | "custom_tool_call_output" | "tool_search_output"
                if kind != "tool_search_output" || item["execution"] == "client" =>
            {
                let call_kind = match kind {
                    "function_call_output" => "function_call",
                    "custom_tool_call_output" => "custom_tool_call",
                    _ => "tool_search_call",
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
                    if kind == "tool_search_output" {
                        anyhow::bail!("工具结果缺少 call_id（{kind}），请重新发送完整上下文");
                    }
                    continue;
                };
                // custom 工具可能继续以同一 call_id 发送异步通知，
                // 保留调用记录，让后续结果仍可找到对应调用。
                let matched = if kind == "custom_tool_call_output" {
                    calls.contains(&(call_kind, id))
                } else {
                    calls.remove(&(call_kind, id))
                };
                if !matched {
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

    fn custom_notification_history() -> Vec<Value> {
        let mut input = vec![json!({"role":"user","content":"run synthetic tasks"})];
        for index in 0..5 {
            let id = format!("call-notify-{index}");
            input.extend([
                json!({
                    "type":"custom_tool_call","call_id":id,"name":"exec",
                    "status":"completed","input":"notify(\"synthetic progress\");"
                }),
                json!({
                    "type":"custom_tool_call_output","call_id":id,
                    "output":format!("Script running with cell ID synthetic-{index}")
                }),
            ]);
        }
        input.extend((0..148).map(|index| {
            json!({
                "type":"custom_tool_call_output",
                "call_id":format!("call-notify-{}", index % 5),"name":"exec",
                "output":format!("synthetic progress {index}")
            })
        }));
        input
    }

    #[test]
    fn custom_tool_notifications_preserve_full_cached_and_compacted_history() {
        for compacted in [false, true] {
            let owner = [1; 32];
            let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
            let headers = vec![("session-id".into(), "custom-notifications".into())];
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
            let mut expected = custom_notification_history();
            if compacted {
                expected.insert(0, json!({"type":"compaction","encrypted_content":"opaque"}));
            }
            let mut full = json!({"input":expected});
            history.prepare(owner, &mut full);
            assert!(
                !history
                    .restore_for_http(owner, &mut full, compacted)
                    .unwrap()
            );
            assert_eq!(full["input"], json!(expected));
            history.observe(&completed("resp-notifications", vec![]));
            drop(history);

            let mut restored = NativeResponsesHistory::with_cache(cache, &headers);
            let delta = vec![
                json!({
                    "type":"custom_tool_call_output","call_id":"call-notify-0",
                    "name":"exec","output":"synthetic final output"
                }),
                json!({"role":"user","content":"continue"}),
            ];
            let mut next = json!({"previous_response_id":"resp-notifications","input":delta});
            restored.prepare(owner, &mut next);
            assert!(restored.can_recover_rejected_continuation(owner, compacted));
            assert!(
                restored
                    .restore_for_http(owner, &mut next, compacted)
                    .unwrap()
            );
            expected.extend(delta);
            assert_eq!(next["input"], json!(expected));
            assert!(next.get("previous_response_id").is_none());
        }
    }

    #[test]
    fn custom_tool_notifications_preserve_pairing_guards() {
        for (input, expected_error) in [
            (
                vec![result(true, "call-1")],
                "工具结果缺少对应的完整调用历史",
            ),
            (
                vec![result(true, "call-1"), call(true, "call-1")],
                "工具结果缺少对应的完整调用历史",
            ),
            (
                vec![call(false, "call-1"), result(true, "call-1")],
                "工具结果缺少对应的完整调用历史",
            ),
            (
                vec![call(true, "call-1"), result(false, "call-1")],
                "工具结果缺少对应的完整调用历史",
            ),
            (
                vec![call(true, "call-1"), call(true, "call-1")],
                "历史中工具调用 ID 重复",
            ),
            (
                vec![
                    call(true, "call-1"),
                    result(true, "call-1"),
                    call(true, "call-1"),
                ],
                "历史中工具调用 ID 重复",
            ),
            (
                vec![
                    call(false, "call-1"),
                    result(false, "call-1"),
                    result(false, "call-1"),
                ],
                "工具结果缺少对应的完整调用历史",
            ),
        ] {
            for restoring in [false, true] {
                let error = validate_native_tool_history(&input, restoring)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(expected_error), "{input:?}: {error}");
            }
        }
    }

    #[tokio::test]
    async fn custom_tool_notifications_reach_native_http_and_sse_in_order() {
        for sse in [false, true] {
            for compacted in [false, true] {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let (mut config, provider, model) =
                    router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
                config.profiles[0].supports_websockets = false;
                config.profiles[0].supports_remote_compaction = compacted;
                let mut expected = custom_notification_history();
                if compacted {
                    expected.insert(0, json!({"type":"compaction","encrypted_content":"opaque"}));
                }
                let split = expected.len() - 74;
                let first = expected[..split].to_vec();
                let later = expected[split..].to_vec();
                let upstream_first = first.clone();
                let upstream = tokio::spawn(async move {
                    for (id, expected) in [
                        ("resp-notify-first", upstream_first),
                        ("resp-notify-later", expected),
                    ] {
                        let (mut stream, _) = listener.accept().await.unwrap();
                        let request = read_http_request(&mut stream).await.unwrap();
                        assert_eq!(request.method, "POST");
                        assert_eq!(request.path, "/v1/responses");
                        let body: Value = serde_json::from_slice(&request.body).unwrap();
                        assert!(body.get("previous_response_id").is_none(), "{body}");
                        assert_eq!(body["input"], json!(expected));
                        let event = completed(id, vec![]);
                        if sse {
                            let response = format!("data: {event}\n\n");
                            stream.write_all(format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                                response.len()
                            ).as_bytes()).await.unwrap();
                        } else {
                            write_json_response(&mut stream, 200, &event["response"])
                                .await
                                .unwrap();
                        }
                    }
                });
                let router = LocalRouter::start(&config).await.unwrap();
                for (previous, input, id) in [
                    (None, first, "resp-notify-first"),
                    (Some("resp-notify-first"), later, "resp-notify-later"),
                ] {
                    let mut client = connect_router_websocket_with_headers(
                        &router.endpoint(),
                        &[("session-id", "custom-notification-http")],
                    )
                    .await;
                    let mut body = json!({
                        "type":"response.create","model":model_alias(&provider, &model),"input":input
                    });
                    if let Some(previous) = previous {
                        body["previous_response_id"] = previous.into();
                    }
                    client
                        .send(WebSocketMessage::Text(body.to_string().into()))
                        .await
                        .unwrap();
                    let event = terminal(&mut client).await;
                    assert_eq!(
                        event["type"], "response.completed",
                        "sse={sse} compacted={compacted}: {event}"
                    );
                    assert_eq!(event["response"]["id"], id, "{event}");
                    client.close(None).await.unwrap();
                }
                upstream.await.unwrap();
                router.stop().await.unwrap();
            }
        }
    }

    #[test]
    fn persisted_context_recovery_rejects_identity_changes_and_empty_reasoning() {
        for allow_native_compaction in [false, true] {
            for output in [
                vec![],
                vec![json!({"type":"reasoning","id":"hidden","summary":[]})],
            ] {
                let recoverable = output.is_empty();
                let mut history = NativeResponsesHistory::default();
                history.prepare([1; 32], &mut json!({"input":"task"}));
                history.observe(&completed("resp-original", output));
                let mut body = json!({"previous_response_id":"resp-original","input":"next"});
                history.prepare([1; 32], &mut body);
                assert_eq!(
                    history.can_recover_rejected_continuation([1; 32], allow_native_compaction),
                    recoverable,
                );
                assert!(
                    !history.can_recover_rejected_continuation([2; 32], allow_native_compaction)
                );
                history.prepare([2; 32], &mut body);
                assert!(
                    !history.can_recover_rejected_continuation([2; 32], allow_native_compaction)
                );
            }
        }
    }

    fn native_client_items() -> Vec<Value> {
        use base64::Engine as _;

        let mut token = vec![0x80];
        token.extend_from_slice(&1_700_000_000u64.to_be_bytes());
        token.extend_from_slice(&[0x11; 16]);
        token.extend_from_slice(&[0x33; 16]);
        token.extend_from_slice(&[0x22; 32]);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token);
        vec![
            json!({"type":"additional_tools","role":"developer","tools":[{
                "type":"namespace","name":"filesystem","tools":[{
                    "type":"function","name":"read_file","parameters":{"type":"object"}
                }]
            }]}),
            json!({"type":"agent_message","author":"/root","recipient":"/root/worker","content":[
                {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
                {"type":"encrypted_content","encrypted_content":token}
            ]}),
            json!({"type":"tool_search_call","execution":"client","call_id":"search-1","arguments":{"goal":"read files"}}),
            json!({"type":"tool_search_output","execution":"client","call_id":"search-1","tools":[{
                "type":"function","name":"read_file","parameters":{"type":"object"}
            }]}),
        ]
    }

    #[test]
    fn native_client_items_allow_initial_continuation_and_cached_replay() {
        let mut items = native_client_items();
        items.extend([
            json!({"type":"additional_tools","role":"developer","tools":[]}),
            json!({"type":"agent_message","message":"worker report"}),
            json!({"type":"agent_message","content":"worker report"}),
            json!({"type":"agent_message","content":{"type":"output_text","text":"worker report"}}),
            json!({"type":"agent_message","content":[{"type":"encrypted_content","encrypted_content":"plaintext task"}]}),
            json!({"type":"tool_search_output","execution":"client","call_id":"search-empty","tools":[]}),
        ]);
        for item in items {
            let owner = [1; 32];
            let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
            let headers = vec![("session-id".into(), "client-history".into())];
            let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
            let mut input = Vec::new();
            if item["type"] == "tool_search_output" {
                input.push(json!({
                    "type":"tool_search_call","execution":"client",
                    "call_id":item["call_id"],"arguments":{}
                }));
            }
            input.push(item.clone());
            let mut first = json!({"input":input});
            history.prepare(owner, &mut first);
            assert!(
                !history.requires_http_for_websocket(&first, false),
                "{item}"
            );
            history.observe(&completed("resp-client", vec![]));
            let mut next = json!({"previous_response_id":"resp-client","input":"next"});
            history.prepare(owner, &mut next);
            assert!(!history.requires_http_for_websocket(&next, true), "{item}");
            let mut reconnected = NativeResponsesHistory::with_cache(cache, &headers);
            reconnected.prepare(owner, &mut next);
            assert!(
                !reconnected.requires_http_for_websocket(&next, false),
                "{item}"
            );
            assert!(reconnected.restore(owner, &mut next).unwrap());
            input.push(json!({"role":"user","content":"next"}));
            assert_eq!(next["input"], json!(input));
            assert!(next.get("previous_response_id").is_none());
        }
    }

    #[test]
    fn incomplete_or_unknown_native_client_items_still_require_http() {
        for item in [
            json!({"type":"additional_tools","tools":[]}),
            json!({"type":"additional_tools","role":"user","tools":[]}),
            json!({"type":"additional_tools","role":"developer"}),
            json!({"type":"additional_tools","role":"developer","tools":[{}]}),
            json!({"type":"agent_message","id":"amsg-reference"}),
            json!({"type":"agent_message","content":[]}),
            json!({"type":"agent_message","content":" "}),
            json!({"type":"agent_message","content":{"type":"encrypted_content","encrypted_content":""}}),
            json!({"type":"agent_message","content":{"type":"encrypted_content","encrypted_content":null}}),
            json!({"type":"agent_message","message":"fallback","content":[
                {"type":"input_text","text":"visible"},{"type":"item_reference","id":"hidden"}
            ]}),
            json!({"type":"agent_message","content":[{"type":"future_content","text":"unknown"}]}),
            json!({"type":"tool_search_call","execution":"server","call_id":"search","arguments":{}}),
            json!({"type":"tool_search_call","call_id":"search","arguments":{}}),
            json!({"type":"tool_search_call","execution":"client","arguments":{}}),
            json!({"type":"tool_search_call","execution":"client","call_id":"search"}),
            json!({"type":"tool_search_call","execution":"client","call_id":"search","arguments":"{}"}),
            json!({"type":"tool_search_output","execution":"server","call_id":"search","tools":[]}),
            json!({"type":"tool_search_output","execution":"client","call_id":"search"}),
            json!({"type":"tool_search_output","execution":"client","call_id":"search","tools":[null]}),
            json!({"type":"item_reference","id":"persisted"}),
            json!({"type":"compaction","encrypted_content":"opaque"}),
            json!({"type":"reasoning","summary":[]}),
            json!({"type":"future_item","content":"unknown"}),
        ] {
            let owner = [1; 32];
            let mut history = NativeResponsesHistory::default();
            history.prepare(owner, &mut json!({"input":"task"}));
            history.observe(&completed("resp-before", vec![]));
            let mut next = json!({"previous_response_id":"resp-before","input":[item.clone()]});
            history.prepare(owner, &mut next);
            assert!(history.requires_http_for_websocket(&next, true), "{item}");
            assert!(history.requires_http_for_websocket(&next, false), "{item}");
            let mut first = json!({"input":[item.clone()]});
            history.prepare(owner, &mut first);
            assert!(history.requires_http_for_websocket(&first, false), "{item}");
        }
    }

    #[test]
    fn restored_client_tool_search_history_requires_matching_calls() {
        let items = native_client_items();
        let search = items[2].clone();
        let output = items[3].clone();
        for (input, expected_error) in [
            (vec![output.clone()], "工具结果缺少对应的完整调用历史"),
            (
                vec![search.clone(), search.clone()],
                "历史中工具调用 ID 重复",
            ),
            (
                vec![search.clone(), output.clone(), output.clone()],
                "工具结果缺少对应的完整调用历史",
            ),
            (
                vec![call(false, "search-1"), output],
                "工具结果缺少对应的完整调用历史",
            ),
            (
                vec![json!({"type":"tool_search_call","execution":"client","arguments":{}})],
                "工具调用缺少 call_id",
            ),
            (
                vec![json!({"type":"tool_search_output","execution":"client","tools":[]})],
                "工具结果缺少 call_id",
            ),
        ] {
            let owner = [1; 32];
            let mut history = NativeResponsesHistory::default();
            history.prepare(owner, &mut json!({"input":input}));
            history.observe(&completed("resp-search", vec![]));
            let mut next = json!({"previous_response_id":"resp-search","input":"next"});
            history.prepare(owner, &mut next);
            let original = next.clone();
            let error = history.restore(owner, &mut next).unwrap_err().to_string();
            assert!(error.contains(expected_error), "{error}");
            assert_eq!(next, original);
        }
        for execution in [json!("server"), Value::Null] {
            let input = vec![
                json!({"type":"tool_search_call","execution":execution}),
                json!({"type":"tool_search_output","execution":execution}),
            ];
            assert!(input.iter().all(native_item_requires_http));
            assert!(validate_native_tool_history(&input, true).is_ok());
        }
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
    async fn official_native_client_history_reuses_ws_and_restores_http_after_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.json");
        std::fs::write(
            &auth_path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"test-access","account_id":"test-account"}}"#,
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let items = native_client_items();
        let first_input = json!([
            {"role":"user","content":"original task"}, items[0], items[1]
        ]);
        let first_expected = first_input.clone();
        let search_output = items[3].clone();
        let (closed_tx, closed_rx) = oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            for round in 0..2 {
                let WebSocketMessage::Text(text) = ws.next().await.unwrap().unwrap() else {
                    panic!("expected response.create");
                };
                let body: Value = serde_json::from_str(&text).unwrap();
                if round == 0 {
                    assert!(body.get("previous_response_id").is_none());
                    assert_eq!(body["input"], first_expected);
                } else {
                    assert_eq!(body["previous_response_id"], "resp-first");
                    assert_eq!(body["input"], json!([items[3]]));
                }
                ws.send(WebSocketMessage::Text(
                    completed(
                        if round == 0 {
                            "resp-first"
                        } else {
                            "resp-second"
                        },
                        if round == 0 {
                            vec![items[2].clone()]
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
            ws.close(None).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(3), ws.next())
                .await
                .unwrap();
            closed_tx.send(()).unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let WebSocketMessage::Text(text) = ws.next().await.unwrap().unwrap() else {
                panic!("expected restored response.create");
            };
            let body: Value = serde_json::from_str(&text).unwrap();
            let mut restored = first_expected.as_array().unwrap().clone();
            restored.extend([
                items[2].clone(),
                items[3].clone(),
                json!({"role":"user","content":"continue after reconnect"}),
            ]);
            assert!(body.get("previous_response_id").is_none());
            assert_eq!(body["input"], json!(restored));
            ws.send(WebSocketMessage::Text(
                completed("resp-reconnected", vec![]).to_string().into(),
            ))
            .await
            .unwrap();
            let WebSocketMessage::Text(text) = ws.next().await.unwrap().unwrap() else {
                panic!("expected incremental response.create");
            };
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["previous_response_id"], "resp-reconnected");
            assert_eq!(body["input"], "after rejection");
            ws.send(WebSocketMessage::Text(
                json!({"type":"error","error":{
                    "type":"invalid_request_error","code":"unsupported_persisted_item_context",
                    "message":"Persisted context cannot be replayed"
                }})
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

            let (mut http, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut http).await.unwrap();
            assert_eq!(request.method, "POST");
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            restored.push(json!({"role":"user","content":"after rejection"}));
            assert!(body.get("previous_response_id").is_none());
            assert_eq!(body["input"], json!(restored));
            let response = format!("data: {}\n\n", completed("resp-http", vec![]));
            http.write_all(format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                response.len()
            ).as_bytes()).await.unwrap();
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
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
        let model = model_alias(&provider, &model);
        let headers = [("session-id", "native-client-history")];
        let mut client = connect_router_websocket_with_headers(&router.endpoint(), &headers).await;
        for (request, expected) in [
            (
                json!({"type":"response.create","model":model,"input":first_input}),
                "resp-first",
            ),
            (
                json!({"type":"response.create","model":model,"previous_response_id":"resp-first","input":[search_output]}),
                "resp-second",
            ),
        ] {
            client
                .send(WebSocketMessage::Text(request.to_string().into()))
                .await
                .unwrap();
            assert_eq!(terminal(&mut client).await["response"]["id"], expected);
        }
        closed_rx.await.unwrap();
        client.close(None).await.unwrap();
        let mut client = connect_router_websocket_with_headers(&router.endpoint(), &headers).await;
        client
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.create","model":model,"previous_response_id":"resp-second",
                    "input":"continue after reconnect"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        assert_eq!(
            terminal(&mut client).await["response"]["id"],
            "resp-reconnected"
        );
        let rejected = json!({
            "type":"response.create","model":model,"previous_response_id":"resp-reconnected",
            "input":"after rejection"
        });
        client
            .send(WebSocketMessage::Text(rejected.to_string().into()))
            .await
            .unwrap();
        assert_eq!(terminal(&mut client).await["response"]["id"], "resp-http");
        upstream.await.unwrap();
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
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
        assert_eq!(failure["type"], "error");
        assert_eq!(failure["error"]["code"], "previous_response_not_found");
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn interrupted_websocket_response_stays_restorable_for_compaction() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider, model) =
            router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
        config.profiles[0].supports_websockets = true;
        config.profiles[0].supports_remote_compaction = true;
        let partial = json!({
            "type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"partial"}]
        });
        let upstream_partial = partial.clone();
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for frame in [
                completed("resp-first", vec![]),
                json!({"type":"response.incomplete","response":{
                    "id":"resp-interrupted","object":"response","status":"incomplete",
                    "incomplete_details":{"reason":"interrupted"},"output":[upstream_partial]
                }}),
                completed("resp-after-interrupt", vec![]),
            ] {
                socket.next().await.unwrap().unwrap();
                socket
                    .send(WebSocketMessage::Text(frame.to_string().into()))
                    .await
                    .unwrap();
            }
            let (mut http, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut http).await.unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert!(body.get("previous_response_id").is_none(), "{body}");
            assert_eq!(
                body["input"],
                json!([
                    {"role":"user","content":"task"}, {"role":"user","content":"next"},
                    partial, {"role":"user","content":"steer"},
                    {"type":"compaction_trigger"}
                ])
            );
            let window = vec![json!({"type":"compaction","encrypted_content":"opaque"})];
            write_json_response(
                &mut http,
                200,
                &completed("resp-compact", window)["response"],
            )
            .await
            .unwrap();
        });
        let router = LocalRouter::start(&config).await.unwrap();
        let mut client = connect_router_websocket(&router.endpoint()).await;
        let model = model_alias(&provider, &model);
        for (previous, input, expected) in [
            (None, json!("task"), "resp-first"),
            (Some("resp-first"), json!("next"), "resp-interrupted"),
            (
                Some("resp-interrupted"),
                json!([{"role":"user","content":"steer"}]),
                "resp-after-interrupt",
            ),
            (
                Some("resp-after-interrupt"),
                json!([{"type":"compaction_trigger"}]),
                "resp-compact",
            ),
        ] {
            let mut request = json!({"type":"response.create","model":model,"input":input});
            if let Some(previous) = previous {
                request["previous_response_id"] = previous.into();
            }
            client
                .send(WebSocketMessage::Text(request.to_string().into()))
                .await
                .unwrap();
            let event = terminal(&mut client).await;
            assert_eq!(event["response"]["id"], expected, "{event}");
        }
        upstream.await.unwrap();
        client.close(None).await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_native_tool_continuation_requests_full_input_before_sending_upstream() {
        for supports_websockets in [false, true] {
            for custom in [false, true] {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let (mut config, provider, model) =
                    router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
                config.profiles[0].supports_websockets = supports_websockets;
                let router = LocalRouter::start(&config).await.unwrap();
                let alias = model_alias(&provider, &model);
                let mut client = connect_router_websocket(&router.endpoint()).await;
                client
                    .send(WebSocketMessage::Text(
                        json!({
                            "type":"response.create", "model":alias, "stream_id":"missing",
                            "previous_response_id":"resp-from-another-socket",
                            "input":[result(custom, "call-1")]
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let error = terminal(&mut client).await;
                assert_eq!(error["type"], "error", "{error}");
                assert_eq!(error["error"]["code"], "previous_response_not_found");
                assert_eq!(error["error"]["type"], "invalid_request_error");
                assert_eq!(error["status"], 400);
                assert!(error.get("response").is_none());
                assert_eq!(error["stream_id"], "missing");
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err()
                );
                client.close(None).await.unwrap();

                let full_input = json!([
                    {"role":"user","content":"task"},
                    call(custom, "call-1"), result(custom, "call-1")
                ]);
                let expected_input = full_input.clone();
                let upstream = tokio::spawn(async move {
                    if supports_websockets {
                        let (stream, _) = listener.accept().await.unwrap();
                        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                        let WebSocketMessage::Text(text) = ws.next().await.unwrap().unwrap() else {
                            panic!("expected complete retry");
                        };
                        let body: Value = serde_json::from_str(&text).unwrap();
                        assert!(body.get("previous_response_id").is_none());
                        assert_eq!(body["input"], expected_input);
                        ws.send(WebSocketMessage::Text(
                            completed("resp-recovered", vec![call(custom, "call-2")])
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                        let WebSocketMessage::Text(text) = ws.next().await.unwrap().unwrap() else {
                            panic!("expected incremental continuation");
                        };
                        let body: Value = serde_json::from_str(&text).unwrap();
                        assert_eq!(body["previous_response_id"], "resp-recovered");
                        assert_eq!(body["input"], json!([result(custom, "call-2")]));
                        ws.send(WebSocketMessage::Text(
                            completed("resp-next", vec![]).to_string().into(),
                        ))
                        .await
                        .unwrap();
                    } else {
                        let mut expected_input = expected_input.as_array().unwrap().clone();
                        for (id, output) in [
                            ("resp-recovered", vec![call(custom, "call-2")]),
                            ("resp-next", vec![]),
                        ] {
                            let (mut stream, _) = listener.accept().await.unwrap();
                            let request = read_http_request(&mut stream).await.unwrap();
                            assert_eq!(request.method, "POST");
                            let body: Value = serde_json::from_slice(&request.body).unwrap();
                            assert!(body.get("previous_response_id").is_none());
                            assert_eq!(body["input"], json!(expected_input));
                            write_json_response(
                                &mut stream,
                                200,
                                &completed(id, output)["response"],
                            )
                            .await
                            .unwrap();
                            expected_input
                                .extend([call(custom, "call-2"), result(custom, "call-2")]);
                        }
                    }
                    assert!(
                        tokio::time::timeout(Duration::from_millis(100), listener.accept())
                            .await
                            .is_err()
                    );
                });
                client = connect_router_websocket(&router.endpoint()).await;
                client
                    .send(WebSocketMessage::Text(
                        json!({
                            "type":"response.create", "model":alias, "stream_id":"retry",
                            "input":full_input
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let recovered = terminal(&mut client).await;
                assert_eq!(recovered["type"], "response.completed", "{recovered}");
                assert_eq!(recovered["response"]["id"], "resp-recovered");
                assert_eq!(recovered["stream_id"], "retry");
                client.send(WebSocketMessage::Text(json!({
                    "type":"response.create", "model":alias, "stream_id":"next",
                    "previous_response_id":"resp-recovered", "input":[result(custom, "call-2")]
                }).to_string().into())).await.unwrap();
                let next = terminal(&mut client).await;
                assert_eq!(next["response"]["id"], "resp-next", "{next}");
                assert_eq!(next["stream_id"], "next");
                upstream.await.unwrap();
                client.close(None).await.unwrap();
                router.stop().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn changed_native_credentials_do_not_request_client_replay() {
        for supports_websockets in [false, true] {
            for reconnect in [false, true] {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let (mut config, provider, model) =
                    router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
                config.profiles[0].supports_websockets = false;
                let router = LocalRouter::start(&config).await.unwrap();
                let alias = model_alias(&provider, &model);
                let headers = [("session-id", "changed-native-credentials")];
                let upstream = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let request = read_http_request(&mut stream).await.unwrap();
                    assert_eq!(request.method, "POST");
                    write_json_response(
                        &mut stream,
                        200,
                        &completed("resp-before-credentials-change", vec![])["response"],
                    )
                    .await
                    .unwrap();
                    listener
                });
                let mut client =
                    connect_router_websocket_with_headers(&router.endpoint(), &headers).await;
                client
                    .send(WebSocketMessage::Text(
                        json!({
                            "type":"response.create", "model":alias, "input":"task"
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                assert_eq!(
                    terminal(&mut client).await["response"]["id"],
                    "resp-before-credentials-change"
                );
                let listener = upstream.await.unwrap();
                // 模拟发送前刷新凭据，线路配置标识本身保持不变。
                let mut snapshot = RouterSnapshot::from_config(&config);
                let route = Arc::make_mut(snapshot.routes.get_mut(&provider).unwrap());
                route.upstream_headers.as_mut().unwrap().insert(
                    AUTHORIZATION,
                    HeaderValue::from_static("Bearer sk-changed-identity"),
                );
                route.supports_websockets = supports_websockets;
                *router.snapshot.write().unwrap() = Arc::new(snapshot);
                if reconnect {
                    client.close(None).await.unwrap();
                    client =
                        connect_router_websocket_with_headers(&router.endpoint(), &headers).await;
                }
                for _ in 0..2 {
                    client.send(WebSocketMessage::Text(json!({
                        "type":"response.create", "model":alias,
                        "previous_response_id":"resp-before-credentials-change", "input":"next"
                    }).to_string().into())).await.unwrap();
                    let failure = terminal(&mut client).await;
                    assert_eq!(failure["type"], "response.failed", "{failure}");
                    assert_eq!(failure["response"]["error"]["code"], "invalid_prompt");
                    assert_eq!(
                        failure["response"]["error"]["codey"]["errorCode"],
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
        }
    }

    #[tokio::test]
    async fn incomplete_native_full_input_is_not_a_recoverable_cache_miss() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider, model) =
            router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
        config.profiles[0].supports_websockets = false;
        let router = LocalRouter::start(&config).await.unwrap();
        let mut client = connect_router_websocket(&router.endpoint()).await;
        for custom in [false, true] {
            client
                .send(WebSocketMessage::Text(
                    json!({
                        "type":"response.create", "model":model_alias(&provider, &model),
                        "input":[result(custom, "missing-call")]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let error = terminal(&mut client).await;
            assert_eq!(error["type"], "response.failed", "{error}");
            assert_eq!(error["response"]["error"]["code"], "invalid_prompt");
            assert_eq!(
                error["response"]["error"]["codey"]["errorCode"],
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
            let error = history.restore([2; 32], &mut next).unwrap_err();
            assert!(!error.is::<MissingResponseHistory>());
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
            let error = history.restore(key, &mut orphan).unwrap_err();
            assert!(!error.is::<MissingResponseHistory>());
            let error = error.to_string();
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
                    .to_string()
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
        let error = expired.restore([1; 32], &mut body).unwrap_err();
        assert!(error.is::<MissingResponseHistory>());
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
                .unwrap()
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
        assert!(cache.get(scope, [1; 32], "large-0").unwrap().is_none());
        assert!(cache.get(scope, [1; 32], "large-1").unwrap().is_some());
        assert!(cache.get(scope, [1; 32], "large-2").unwrap().is_some());
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
    fn known_native_identity_changes_remain_fatal_after_reprepare() {
        let (config, provider, model) = router_config("http://127.0.0.1:9/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        let route = snapshot.routes[&provider].as_ref();
        let original_auth = UpstreamWebSocketAuthIdentity {
            authorization: Some([1; 32]),
            account_id: Some([2; 32]),
        };
        for scoped in [false, true] {
            for changed in 0..4 {
                let headers = if scoped {
                    vec![("session-id".into(), "identity-change".into())]
                } else {
                    vec![]
                };
                let cache = Arc::new(Mutex::new(NativeHistoryCache::default()));
                let mut history = NativeResponsesHistory::with_cache(Arc::clone(&cache), &headers);
                history.prepare_for_route(
                    route,
                    original_auth,
                    &mut json!({"model":model,"input":"task"}),
                );
                history.observe(&completed("resp-original-identity", vec![]));
                let mut changed_route = route.clone();
                let mut changed_auth = original_auth;
                let mut next = json!({
                    "model":model,"previous_response_id":"resp-original-identity","input":"next"
                });
                match changed {
                    0 => changed_route.provider_id.push_str("-other"),
                    1 => next["model"] = "other-model".into(),
                    2 => changed_auth.authorization = Some([3; 32]),
                    _ => changed_auth.account_id = Some([4; 32]),
                }
                for _ in 0..2 {
                    let owner = history.prepare_for_route(&changed_route, changed_auth, &mut next);
                    let error = history.restore(owner, &mut next).unwrap_err();
                    assert!(!error.is::<MissingResponseHistory>());
                    assert!(error.to_string().contains("上游身份已变化"), "{error}");
                }
                if scoped {
                    let mut separate = NativeResponsesHistory::with_cache(cache, &headers);
                    for _ in 0..2 {
                        let owner =
                            separate.prepare_for_route(&changed_route, changed_auth, &mut next);
                        let error = separate.restore(owner, &mut next).unwrap_err();
                        assert!(!error.is::<MissingResponseHistory>());
                        assert!(error.to_string().contains("上游身份已变化"), "{error}");
                    }
                }
                next.as_object_mut().unwrap().remove("previous_response_id");
                let owner = history.prepare_for_route(&changed_route, changed_auth, &mut next);
                assert!(!history.restore(owner, &mut next).unwrap());
                history.observe(&completed("resp-new-identity", vec![]));
                next["previous_response_id"] = "resp-new-identity".into();
                history.prepare_for_route(&changed_route, changed_auth, &mut next);
                assert!(history.restore(owner, &mut next).unwrap());
            }
        }
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
            let error = separate
                .restore_for_http(owner, &mut separate_body, true)
                .unwrap_err();
            assert!(!error.is::<MissingResponseHistory>());
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
                let owner = history.prepare_for_route(&route, next_auth, &mut next);
                let error = history
                    .restore_for_http(owner, &mut next, true)
                    .unwrap_err();
                assert!(!error.is::<MissingResponseHistory>());
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
                assert!(!restored.requires_http_for_websocket(&next, true));
                assert_eq!(
                    restored.requires_http_for_websocket(&next, false),
                    item["type"] != "reasoning"
                );
                assert!(restored.restore_for_http(owner, &mut next, true).unwrap());
                assert_eq!(next["input"][1], item);
                assert!(next.get("previous_response_id").is_none());
            }
        }
    }

    #[test]
    fn native_websocket_continuation_checks_new_input_and_upstream_rejection() {
        let owner = [1; 32];
        let mut history = NativeResponsesHistory::default();
        let hosted = json!({"type":"web_search_call","id":"search","status":"completed"});
        history.prepare(owner, &mut json!({"input":"task"}));
        history.observe(&completed("resp-hosted", vec![hosted.clone()]));
        let mut next = json!({"previous_response_id":"resp-hosted","input":"next"});
        history.prepare(owner, &mut next);
        assert!(!history.requires_http_for_websocket(&next, true));
        assert!(history.requires_http_for_websocket(&next, false));
        for input in [
            json!([hosted]),
            json!([{"type":"compaction","encrypted_content":"opaque"}]),
            json!([{"type":"reasoning","summary":[]}]),
        ] {
            let mut unsafe_input = json!({"previous_response_id":"resp-hosted","input":input});
            history.prepare(owner, &mut unsafe_input);
            assert!(history.requires_http_for_websocket(&unsafe_input, true));
            let mut full = json!({"input":input});
            history.prepare(owner, &mut full);
            assert!(history.requires_http_for_websocket(&full, true));
        }
        history.prepare(owner, &mut next);
        history.observe(
            &json!({"type":"error","error":{"code":"unsupported_persisted_item_context"}}),
        );
        history.prepare(owner, &mut next);
        assert!(history.requires_http_for_websocket(&next, true));
        assert!(history.requires_http_for_websocket(&next, false));
        history.prepare(owner, &mut json!({"input":"independent task"}));
        assert!(!history.requires_http());
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
    fn failed_compaction_keeps_the_last_successful_history() {
        for status in ["failed", "incomplete"] {
            let owner = [1; 32];
            let mut history = NativeResponsesHistory::default();
            history.prepare(owner, &mut json!({"input":"original task"}));
            history.observe(&completed("resp-before", vec![]));
            history.prepare(owner, &mut json!({"previous_response_id":"resp-before","input":[{"type":"compaction_trigger"}]}));
            history.observe(&json!({"type":format!("response.{status}"),"response":{"id":"resp-failed","status":status,"output":[{"type":"compaction","encrypted_content":"partial"}]}}));
            let mut next = json!({"previous_response_id":"resp-before","input":"continue"});
            history.prepare(owner, &mut next);
            assert!(history.restore_for_http(owner, &mut next, true).unwrap());
            assert_eq!(
                next["input"],
                json!([{"role":"user","content":"original task"},{"role":"user","content":"continue"}])
            );
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
            assert_eq!(
                history.requires_http_for_websocket(&next, true),
                requires_http
            );
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
        assert!(inherited.requires_http_for_websocket(&next, true));
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
