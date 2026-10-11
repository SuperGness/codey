use super::*;

const MAX_STEERS: usize = 8;
const MAX_STEER_BYTES: usize = 8 * 1024 * 1024;
const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

struct PendingSteer {
    value: Value,
    stream_id: Option<String>,
    notified: bool,
    _permit: Option<OwnedSemaphorePermit>,
}

/// 补充输入只属于当前连接、当前响应，等待当前响应及客户端工具完成后续接。
#[derive(Default)]
pub(crate) struct SteeringState {
    settings: Option<Value>,
    settings_permit: Option<OwnedSemaphorePermit>,
    response_id: Option<String>,
    completed: Option<bool>,
    required: Vec<Value>,
    unsupported_output: bool,
    pending: Vec<PendingSteer>,
    submitted: Vec<PendingSteer>,
    continuation_offered: bool,
    pending_bytes: usize,
    sequence: u64,
    pub(crate) controls_sent: bool,
    pub(crate) stream_id: Option<String>,
}

impl SteeringState {
    fn event(&mut self, kind: &str, steer: Value) -> Value {
        self.controls_sent = true;
        self.sequence = self.sequence.saturating_add(1);
        let mut event = json!({"type":kind,"sequence_number":self.sequence,"steer":steer});
        if let Some(stream_id) = &self.stream_id {
            event["stream_id"] = stream_id.clone().into();
        }
        event
    }

    pub(crate) fn sequence_response_event(&mut self, event: &mut Value) -> bool {
        if !self.controls_sent || event.get("sequence_number").is_none() {
            return false;
        }
        let next = self
            .sequence
            .saturating_add(1)
            .max(event["sequence_number"].as_u64().unwrap_or(0));
        self.sequence = next;
        event["sequence_number"] = next.into();
        true
    }

    fn failure(&mut self, steer: Value, code: &str, message: &str) -> Value {
        let mut event = self.event("response.steer.failed", steer);
        event["error"] = json!({"type":"invalid_request_error","code":code,"message":message});
        event
    }

    fn fail_pending(&mut self, code: &str, message: &str) -> Vec<Value> {
        self.pending_bytes = 0;
        let pending = std::mem::take(&mut self.pending)
            .into_iter()
            .chain(std::mem::take(&mut self.submitted));
        pending
            .map(|pending| {
                let mut event = self.failure(pending.value, code, message);
                event.as_object_mut().unwrap().remove("stream_id");
                if let Some(stream_id) = pending.stream_id {
                    event["stream_id"] = stream_id.into();
                }
                event
            })
            .collect()
    }

    pub(crate) fn begin_request(
        &mut self,
        body: &mut Value,
        budget: &Arc<Semaphore>,
    ) -> Result<Vec<Value>> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            if self.completed == Some(true)
                && body.get("previous_response_id").and_then(Value::as_str)
                    == self.response_id.as_deref()
            {
                let supplied = body
                    .get("input")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let matches = |item: &Value, required: &Value| {
                    let key = if required["type"] == "mcp_approval_response" {
                        "approval_request_id"
                    } else {
                        "call_id"
                    };
                    item["type"] == required["type"] && item[key] == required[key]
                };
                anyhow::ensure!(
                    self.required.iter().all(|required| {
                        let mut matching = supplied.iter().filter(|item| matches(item, required));
                        matching.next().is_some_and(|item| {
                            if required["type"] == "mcp_approval_response" {
                                item["approve"].is_boolean()
                            } else {
                                item.get("output").is_some()
                            }
                        }) && matching.next().is_none()
                    }) && supplied
                        .iter()
                        .filter(|item| matches!(
                            item["type"].as_str(),
                            Some(
                                "function_call_output"
                                    | "custom_tool_call_output"
                                    | "mcp_approval_response"
                            )
                        ))
                        .all(|item| self.required.iter().any(|required| matches(item, required))),
                    "补充消息需要完整且不重复的工具结果或审批结果"
                );
                let mut input = self.take_input();
                append_input(
                    &mut input,
                    body.as_object_mut()
                        .and_then(|body| body.remove("input"))
                        .unwrap_or(Value::Null),
                );
                body["input"] = input.into();
            } else {
                events = self.fail_pending("response_not_found", "目标响应已切换，补充消息未应用");
            }
        }
        self.settings = None;
        self.settings_permit = None;
        self.response_id = None;
        self.completed = None;
        self.required.clear();
        self.unsupported_output = false;
        self.continuation_offered = false;
        self.stream_id = body
            .get("stream_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        // 先计算借用字段的大小并申请预算，避免复制整段会话历史。
        if let Some(object) = body.as_object() {
            let settings: std::collections::BTreeMap<_, _> = object
                .iter()
                .filter(|(key, _)| {
                    !matches!(key.as_str(), "input" | "previous_response_id" | "type")
                })
                .collect();
            let bytes = serde_json::to_vec(&settings)?;
            if bytes.len() <= MAX_SETTINGS_BYTES
                && let Ok(permit) = acquire_request_body_budget(budget, bytes.len())
            {
                self.settings = Some(serde_json::from_slice(&bytes)?);
                self.settings_permit = permit;
            }
        }
        Ok(events)
    }

    pub(crate) fn accept(&mut self, body: &Value, budget: &Arc<Semaphore>) -> Value {
        let steer = json!({"id":format!("steer_{}", Uuid::new_v4().simple()),
            "previous_response_id":body.get("previous_response_id").cloned().unwrap_or(Value::Null),
            "input":body.get("input").cloned().unwrap_or(Value::Null)});
        if !valid_steer(body) {
            return self.failure(
                steer,
                "invalid_input",
                "补充消息只接受目标响应 ID 和用户输入",
            );
        }
        if self.response_id.as_deref() != body["previous_response_id"].as_str()
            || self.completed == Some(false)
        {
            return self.failure(
                steer,
                "response_not_found",
                "当前连接上没有可续接的目标响应",
            );
        }
        if self.settings.is_none() || self.unsupported_output {
            return self.failure(
                steer,
                "steering_not_supported",
                "当前请求无法安全续接补充消息",
            );
        }
        let bytes = body.to_string().len();
        if self.pending.len() >= MAX_STEERS
            || self.pending_bytes.saturating_add(bytes) > MAX_STEER_BYTES
        {
            return self.failure(
                steer,
                "too_many_pending_steers",
                "待处理补充消息已达上限，请先等待当前响应或返回工具结果",
            );
        }
        let Ok(permit) = acquire_request_body_budget(budget, bytes) else {
            return self.failure(
                steer,
                "too_many_pending_steers",
                "补充消息缓冲预算不足，请稍后重试",
            );
        };
        let acknowledgement =
            json!({"id":steer["id"],"previous_response_id":steer["previous_response_id"]});
        self.pending.push(PendingSteer {
            value: steer,
            stream_id: self.stream_id.clone(),
            notified: false,
            _permit: permit,
        });
        self.pending_bytes += bytes;
        self.event("response.steer.accepted", acknowledgement)
    }

    pub(crate) fn observe(&mut self, event: &Value) {
        if event["type"] == "response.created" || event["type"] == "response.completed" {
            // 上游已开始处理续接；此后的响应失败由普通响应终态表达。
            self.submitted.clear();
        }
        self.sequence = self
            .sequence
            .max(event["sequence_number"].as_u64().unwrap_or(0));
        if let Some(id) =
            responses_event_response_id(event).filter(|id| !id.is_empty() && id.len() <= 1024)
        {
            self.response_id = Some(id.to_owned());
        }
        if event["type"] == "response.output_item.done" {
            self.require_item(&event["item"]);
        }
        if responses_event_is_terminal(event) {
            self.completed = Some(
                event["type"] == "response.completed"
                    && event["response"]
                        .get("status")
                        .and_then(Value::as_str)
                        .is_none_or(|s| s == "completed"),
            );
            if let Some(output) = event["response"]["output"].as_array() {
                for item in output {
                    self.require_item(item);
                }
            }
        }
    }

    fn require_item(&mut self, item: &Value) {
        let kind = item["type"].as_str().unwrap_or("");
        let output_kind = match kind {
            "function_call" => "function_call_output",
            "custom_tool_call" => "custom_tool_call_output",
            "mcp_approval_request" => "mcp_approval_response",
            // 其他客户端工具的输出形状不作猜测。
            "computer_call" | "local_shell_call" | "shell_call" | "apply_patch_call" => {
                self.unsupported_output = true;
                return;
            }
            _ => return,
        };
        let (key, id) = if kind == "mcp_approval_request" {
            ("approval_request_id", &item["id"])
        } else {
            ("call_id", &item["call_id"])
        };
        if id
            .as_str()
            .is_none_or(|id| id.is_empty() || id.len() > 1024)
            || self.required.len() >= 128
        {
            self.unsupported_output = true;
            return;
        }
        let mut required = json!({"type":output_kind});
        required[key] = id.clone();
        if let Some(name) = item["name"].as_str().filter(|name| name.len() <= 1024) {
            required["name"] = name.into();
        }
        if !self
            .required
            .iter()
            .any(|existing| existing["type"] == required["type"] && existing[key] == required[key])
        {
            self.required.push(required);
        }
    }

    pub(crate) fn notifications(&mut self) -> Vec<Value> {
        if self.completed == Some(false) || self.unsupported_output {
            return self.fail_pending(
                "steering_not_supported",
                "目标响应未成功完成或需要不兼容的工具结果，补充消息未应用",
            );
        }
        if self.completed != Some(true) || self.required.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::new();
        for pending in &mut self.pending {
            if !pending.notified {
                pending.notified = true;
                self.sequence = self.sequence.saturating_add(1);
                let mut event = json!({"type":"response.steer.pending","sequence_number":self.sequence,
                    "steer":{"id":pending.value["id"],"previous_response_id":pending.value["previous_response_id"]},
                    "reason":"waiting_for_required_input","required_input":self.required});
                if let Some(stream_id) = &pending.stream_id {
                    event["stream_id"] = stream_id.clone().into();
                }
                events.push(event);
            }
        }
        events
    }

    fn take_input(&mut self) -> Vec<Value> {
        self.pending_bytes = 0;
        let mut input = Vec::new();
        for pending in std::mem::take(&mut self.pending) {
            append_input(&mut input, pending.value["input"].clone());
            // 在请求准入成功、上游创建响应之前，保留原输入及预算以便明确退回。
            self.submitted.push(pending);
        }
        input
    }

    pub(crate) fn take_continuation(&mut self) -> Option<Value> {
        if self.completed != Some(true)
            || self.unsupported_output
            || !self.required.is_empty()
            || self.pending.is_empty()
            || self.continuation_offered
        {
            return None;
        }
        let mut body = self.settings.as_ref()?.clone();
        body["type"] = "response.create".into();
        body["previous_response_id"] = self.response_id.clone()?.into();
        body["input"] = json!([]);
        self.continuation_offered = true;
        Some(body)
    }
}

fn append_input(target: &mut Vec<Value>, input: Value) {
    match input {
        Value::String(text) => target.push(json!({"role":"user","content":text})),
        Value::Array(items) => target.extend(items),
        Value::Null => {}
        other => target.push(other),
    }
}

fn valid_steer(body: &Value) -> bool {
    let Some(object) = body.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "previous_response_id" | "input"))
        || body["type"] != "response.steer"
        || body["previous_response_id"]
            .as_str()
            .is_none_or(|id| id.is_empty() || id.len() > 1024)
    {
        return false;
    }
    match &body["input"] {
        Value::String(_) => true,
        Value::Array(items) if !items.is_empty() => items.iter().all(|item| {
            item.as_object().is_some_and(|item| {
                item.keys()
                    .all(|key| matches!(key.as_str(), "type" | "role" | "content"))
            }) && item["role"] == "user"
                && item.get("type").is_none_or(|kind| kind == "message")
                && match &item["content"] {
                    Value::String(_) => true,
                    Value::Array(parts) if !parts.is_empty() => {
                        parts.iter().all(|part| match part["type"].as_str() {
                            Some("input_text") => part["text"].is_string(),
                            Some("input_image") => {
                                part["image_url"].is_string() || part["file_id"].is_string()
                            }
                            Some("input_file") => {
                                part["file_data"].is_string()
                                    || part["file_id"].is_string()
                                    || part["file_url"].is_string()
                            }
                            _ => false,
                        })
                    }
                    _ => false,
                }
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (SteeringState, Arc<Semaphore>) {
        let budget = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
        let mut state = SteeringState::default();
        state
            .begin_request(
                &mut json!({"model":"test","input":"old","instructions":"keep"}),
                &budget,
            )
            .unwrap();
        state.observe(&json!({"type":"response.created","response":{"id":"parent"}}));
        (state, budget)
    }

    fn steer(input: &str) -> Value {
        json!({"type":"response.steer","previous_response_id":"parent","input":input})
    }

    #[test]
    fn steering_bounds_queue_and_releases_budget() {
        let (mut state, budget) = setup();
        let baseline = budget.available_permits();
        for _ in 0..MAX_STEERS {
            assert_eq!(
                state.accept(&steer("update"), &budget)["type"],
                "response.steer.accepted"
            );
        }
        assert!(budget.available_permits() < baseline);
        assert_eq!(
            state.accept(&steer("overflow"), &budget)["error"]["code"],
            "too_many_pending_steers"
        );
        drop(state);
        assert_eq!(budget.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
    }

    #[test]
    fn steering_failure_returns_each_input_and_never_continues() {
        let (mut state, budget) = setup();
        let accepted = state.accept(&steer("update"), &budget);
        state.observe(&json!({"type":"response.failed","response":{"id":"parent"}}));
        let failures = state.notifications();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0]["steer"]["id"], accepted["steer"]["id"]);
        assert_eq!(failures[0]["steer"]["input"], "update");
        assert!(state.take_continuation().is_none());
        assert!(state.notifications().is_empty());
    }

    #[test]
    fn steering_rejects_non_user_input_and_setting_changes() {
        let (mut state, budget) = setup();
        for input in [
            json!([]),
            json!([{"role":"system","content":"override"}]),
            json!([{"type":"function_call_output","call_id":"fake","output":"x"}]),
        ] {
            let mut request = steer("unused");
            request["input"] = input;
            assert_eq!(
                state.accept(&request, &budget)["error"]["code"],
                "invalid_input"
            );
        }
        let mut request = steer("update");
        request["model"] = "other".into();
        assert_eq!(
            state.accept(&request, &budget)["error"]["code"],
            "invalid_input"
        );
    }

    #[test]
    fn steering_keeps_input_until_all_tools_return_and_isolates_new_parent() {
        let (mut state, budget) = setup();
        state.accept(&steer("update"), &budget);
        state.observe(
            &json!({"type":"response.completed","response":{"id":"parent","output":[
                {"type":"function_call","call_id":"one","name":"lookup"},
                {"type":"custom_tool_call","call_id":"two","name":"patch"}
            ]}}),
        );
        assert_eq!(state.notifications().len(), 1);
        assert!(state.notifications().is_empty());
        let mut incomplete = json!({"model":"test","previous_response_id":"parent","input":[{"type":"function_call_output","call_id":"one","output":"ok"}]});
        assert!(state.begin_request(&mut incomplete, &budget).is_err());
        assert_eq!(state.pending.len(), 1);
        let failures = state
            .begin_request(
                &mut json!({"model":"test","input":"different task"}),
                &budget,
            )
            .unwrap();
        assert_eq!(failures[0]["steer"]["input"], "update");
        assert!(state.pending.is_empty());
    }

    #[test]
    fn steering_admission_failures_return_input_with_original_lane() {
        for before_begin in [true, false] {
            let (mut state, budget) = setup();
            state.stream_id = Some("original-lane".into());
            let accepted = state.accept(&steer("retained input"), &budget);
            state.observe(
                &json!({"type":"response.completed","response":{"id":"parent","output":[]}}),
            );
            let mut body = state.take_continuation().unwrap();
            assert_eq!(state.pending.len(), 1);
            assert!(state.take_continuation().is_none());
            if !before_begin {
                body["stream_id"] = "new-lane".into();
                state.begin_request(&mut body, &budget).unwrap();
                assert_eq!(body["input"][0]["content"], "retained input");
                assert_eq!(state.submitted.len(), 1);
            }
            state.observe(
                &json!({"type":"response.failed","response":{"id":"local-admission-error"}}),
            );
            let events = state.notifications();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["steer"]["id"], accepted["steer"]["id"]);
            assert_eq!(events[0]["steer"]["input"], "retained input");
            assert_eq!(events[0]["stream_id"], "original-lane");
            assert!(state.notifications().is_empty());
        }
    }

    #[test]
    fn steering_budget_rejection_does_not_finish_active_response() {
        let (mut state, _) = setup();
        let budget = Arc::new(Semaphore::new(0));
        let event = state.accept(&steer("retain on client"), &budget);
        assert_eq!(event["error"]["code"], "too_many_pending_steers");
        assert_eq!(event["steer"]["input"], "retain on client");
        assert_eq!(state.response_id.as_deref(), Some("parent"));
        assert_eq!(state.completed, None);
        assert!(state.pending.is_empty());
    }
}
