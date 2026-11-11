use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::Value;

use super::{MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT, SessionLifecycleStatus, TurnConfiguration};

const MAX_CACHED_TURN_CONFIGURATIONS_PER_ROLLOUT: usize = MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT * 2;

#[derive(Debug, Clone, Default)]
pub(super) struct ParsedRolloutEvents {
    #[cfg(test)]
    pending_approvals: Vec<(String, String)>,
    pub(super) started_turns: Vec<String>,
    pub(super) aborted_turns: Vec<String>,
    pub(super) completed_turns: Vec<(String, u128, Option<i64>, Option<String>)>,
    pub(super) snapshot_replay_turns: HashSet<String>,
    pub(super) turn_configurations: HashMap<String, TurnConfiguration>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RolloutParseState {
    is_subagent: bool,
    forked_session_id: Option<String>,
    replaying_fork_history: bool,
    current_turn_id: String,
    waiting_calls: HashMap<String, String>,
    async_questions: HashMap<String, HashSet<usize>>,
    terminal_turns: HashSet<String>,
    terminal_turn_order: VecDeque<String>,
    active_turns: HashSet<String>,
    turn_configuration_order: VecDeque<String>,
    latest_terminal: SessionLifecycleStatus,
    events: ParsedRolloutEvents,
}

impl RolloutParseState {
    pub(super) fn is_subagent(&self) -> bool {
        self.is_subagent
    }

    pub(super) fn events(&self) -> &ParsedRolloutEvents {
        &self.events
    }

    /// Consumes whole lines from `chunk`, returning how many bytes were taken.
    /// A trailing partial line is only consumed once it parses as a complete
    /// JSON record, so a rollout caught mid-write resumes cleanly.
    pub(super) fn consume(&mut self, chunk: &str) -> usize {
        if self.is_subagent {
            return chunk.len();
        }
        let mut consumed = 0usize;
        for segment in chunk.split_inclusive('\n') {
            let is_complete_line = segment.ends_with('\n');
            let line = segment.trim_end_matches(['\n', '\r']);
            if !is_complete_line {
                if line.trim().is_empty() {
                    break;
                }
                let Ok(record) = serde_json::from_str::<Value>(line) else {
                    break;
                };
                consumed += segment.len();
                self.apply(&record);
                break;
            }
            consumed += segment.len();
            if self.line_may_affect_state(line)
                && let Ok(record) = serde_json::from_str::<Value>(line)
            {
                self.apply(&record);
            }
            if self.is_subagent {
                return chunk.len();
            }
        }
        consumed
    }

    fn line_may_affect_state(&self, line: &str) -> bool {
        let bytes = line.as_bytes();
        if memchr::memmem::find(bytes, b"\\u").is_some() {
            return true;
        }
        if memchr::memmem::find(bytes, b"session_meta").is_some()
            || memchr::memmem::find(bytes, b"turn_context").is_some()
            || memchr::memmem::find(bytes, b"event_msg").is_some()
        {
            return true;
        }
        if memchr::memmem::find(bytes, b"function_call").is_some()
            && (memchr::memmem::find(bytes, b"request_").is_some()
                || memchr::memmem::find(bytes, b"exec_command").is_some()
                || ((!self.waiting_calls.is_empty() || !self.async_questions.is_empty())
                    && memchr::memmem::find(bytes, b"function_call_output").is_some()))
        {
            return true;
        }
        !self.async_questions.is_empty()
            && memchr::memmem::find(bytes, b"send_user_message_question_reply").is_some()
    }

    fn apply(&mut self, record: &Value) {
        let Some(payload) = record.get("payload") else {
            return;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if is_subagent_payload(payload) {
                    self.is_subagent = true;
                }
                self.observe_session_meta(payload);
            }
            Some("turn_context") => {
                if let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) {
                    self.current_turn_id = turn_id.to_string();
                    let model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let reasoning_effort = payload
                        .get("effort")
                        .or_else(|| payload.get("reasoning_effort"))
                        .or_else(|| payload.get("reasoningEffort"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if !model.is_empty() || !reasoning_effort.is_empty() {
                        self.remember_turn_configuration(
                            turn_id.to_string(),
                            TurnConfiguration {
                                model,
                                reasoning_effort,
                            },
                        );
                    }
                }
            }
            Some("event_msg") => {
                let Some(turn_id) = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|turn_id| !turn_id.is_empty())
                else {
                    return;
                };
                match payload.get("type").and_then(Value::as_str) {
                    Some("task_started") if self.active_turns.insert(turn_id.to_string()) => {
                        self.events.started_turns.push(turn_id.to_string());
                    }
                    Some("task_complete") => {
                        let error = task_completion_error(payload);
                        self.latest_terminal = if error.is_some() {
                            SessionLifecycleStatus::Error
                        } else {
                            SessionLifecycleStatus::Idle
                        };
                        if self.finish_turn(turn_id) {
                            if self.replaying_fork_history {
                                self.events
                                    .snapshot_replay_turns
                                    .insert(turn_id.to_string());
                            }
                            self.events.completed_turns.push((
                                turn_id.to_string(),
                                payload
                                    .get("duration_ms")
                                    .and_then(Value::as_u64)
                                    .unwrap_or_default() as u128,
                                payload.get("completed_at").and_then(Value::as_i64),
                                error,
                            ));
                            self.remember_terminal_turn(turn_id);
                        }
                    }
                    Some("turn_aborted") => {
                        self.latest_terminal = SessionLifecycleStatus::Idle;
                        if self.finish_turn(turn_id) {
                            if self.replaying_fork_history {
                                self.events
                                    .snapshot_replay_turns
                                    .insert(turn_id.to_string());
                            }
                            self.events.aborted_turns.push(turn_id.to_string());
                            self.remember_terminal_turn(turn_id);
                        }
                    }
                    _ => {}
                }
            }
            Some("response_item") => match payload.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    if !function_call_requires_approval(payload) {
                        return;
                    }
                    let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
                        return;
                    };
                    let turn_id = payload
                        .get("internal_chat_message_metadata_passthrough")
                        .and_then(|metadata| metadata.get("turn_id"))
                        .and_then(Value::as_str)
                        .unwrap_or(&self.current_turn_id);
                    if payload.get("name").and_then(Value::as_str)
                        == Some("request_user_input_async")
                    {
                        let arguments = match payload.get("arguments") {
                            Some(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
                            arguments => arguments.cloned(),
                        };
                        let Some(questions) = arguments
                            .as_ref()
                            .and_then(|args| args.get("questions"))
                            .and_then(Value::as_array)
                            .filter(|questions| !questions.is_empty())
                        else {
                            return;
                        };
                        self.async_questions
                            .insert(call_id.to_string(), (0..questions.len()).collect());
                    }
                    self.waiting_calls
                        .insert(call_id.to_string(), turn_id.to_string());
                }
                Some("function_call_output") => {
                    if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                        if self.async_questions.contains_key(call_id) {
                            let output = match payload.get("output") {
                                Some(Value::String(text)) => {
                                    serde_json::from_str::<Value>(text).ok()
                                }
                                output => output.cloned(),
                            };
                            // Acceptance publishes the question; the user's reply arrives separately.
                            if output
                                .as_ref()
                                .and_then(|value| value.get("accepted"))
                                .and_then(Value::as_bool)
                                == Some(true)
                            {
                                return;
                            }
                            self.async_questions.remove(call_id);
                        }
                        self.waiting_calls.remove(call_id);
                    }
                }
                Some("message") if payload.get("role").and_then(Value::as_str) == Some("user") => {
                    self.resolve_async_questions(payload);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn resolve_async_questions(&mut self, payload: &Value) {
        if self.async_questions.is_empty() {
            return;
        }
        for content in payload
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(text) = content
                .get("text")
                .and_then(Value::as_str)
                .and_then(|text| {
                    text.trim()
                        .strip_prefix("<send_user_message_question_reply>")
                })
                .and_then(|text| text.strip_suffix("</send_user_message_question_reply>"))
            else {
                continue;
            };
            let Ok(replies) = serde_json::from_str::<Vec<Value>>(text) else {
                continue;
            };
            for reply in replies {
                let Some(id) = reply.get("questionItemId").and_then(Value::as_str) else {
                    continue;
                };
                let Ok((tool, call_id, index)) =
                    serde_json::from_str::<(String, String, usize)>(id)
                else {
                    continue;
                };
                if tool != "request_user_input_async" {
                    continue;
                }
                if let Some(questions) = self.async_questions.get_mut(&call_id) {
                    questions.remove(&index);
                    if questions.is_empty() {
                        self.async_questions.remove(&call_id);
                        self.waiting_calls.remove(&call_id);
                    }
                }
            }
        }
    }

    fn finish_turn(&mut self, turn_id: &str) -> bool {
        self.active_turns.remove(turn_id);
        self.waiting_calls
            .retain(|_, waiting_turn_id| waiting_turn_id != turn_id);
        self.async_questions
            .retain(|call_id, _| self.waiting_calls.contains_key(call_id));
        self.terminal_turns.insert(turn_id.to_string())
    }

    fn remember_terminal_turn(&mut self, turn_id: &str) {
        self.terminal_turn_order.push_back(turn_id.to_string());
        while self.terminal_turn_order.len() > MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT {
            let Some(oldest) = self.terminal_turn_order.pop_front() else {
                break;
            };
            self.terminal_turns.remove(&oldest);
            self.events
                .started_turns
                .retain(|candidate| candidate != &oldest);
            self.events
                .completed_turns
                .retain(|(candidate, _, _, _)| candidate != &oldest);
            self.events
                .aborted_turns
                .retain(|candidate| candidate != &oldest);
            self.events.snapshot_replay_turns.remove(&oldest);
            self.events.turn_configurations.remove(&oldest);
            self.turn_configuration_order
                .retain(|candidate| candidate != &oldest);
        }
    }

    fn remember_turn_configuration(&mut self, turn_id: String, configuration: TurnConfiguration) {
        let is_new = !self.events.turn_configurations.contains_key(&turn_id);
        self.events
            .turn_configurations
            .insert(turn_id.clone(), configuration);
        if is_new {
            self.turn_configuration_order.push_back(turn_id);
        }
        while self.turn_configuration_order.len() > MAX_CACHED_TURN_CONFIGURATIONS_PER_ROLLOUT {
            let removable_index = self
                .turn_configuration_order
                .iter()
                .position(|candidate| {
                    !self.active_turns.contains(candidate)
                        && !self
                            .waiting_calls
                            .values()
                            .any(|waiting_turn_id| waiting_turn_id == candidate)
                })
                .unwrap_or(0);
            if let Some(oldest) = self.turn_configuration_order.remove(removable_index) {
                self.events.turn_configurations.remove(&oldest);
            }
        }
    }

    fn observe_session_meta(&mut self, payload: &Value) {
        let session_id = payload
            .get("id")
            .or_else(|| payload.get("session_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty());
        let is_fork_header = payload
            .get("forked_from_id")
            .and_then(Value::as_str)
            .is_some_and(|parent_id| !parent_id.trim().is_empty());

        if self.forked_session_id.is_none() && is_fork_header {
            self.forked_session_id = session_id.map(ToString::to_string);
        }
        if let (Some(forked_session_id), Some(session_id)) =
            (self.forked_session_id.as_deref(), session_id)
        {
            // A fork rollout starts with its new header, replays one or more
            // ancestor session snapshots, then writes the new header again
            // before live events continue.
            self.replaying_fork_history = session_id != forked_session_id;
        }
    }

    pub(super) fn pending_approvals(&self) -> Vec<(String, String)> {
        let mut pending_approvals = self
            .waiting_calls
            .iter()
            .filter(|(_, turn_id)| {
                !turn_id.is_empty() && !self.terminal_turns.contains(turn_id.as_str())
            })
            .map(|(call_id, turn_id)| (turn_id.clone(), call_id.clone()))
            .collect::<Vec<_>>();
        pending_approvals.sort();
        pending_approvals
    }

    pub(super) fn lifecycle_status(
        &self,
        pending_approvals: &[(String, String)],
    ) -> SessionLifecycleStatus {
        if !pending_approvals.is_empty() {
            SessionLifecycleStatus::Waiting
        } else if !self.active_turns.is_empty() {
            SessionLifecycleStatus::Running
        } else {
            self.latest_terminal
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> Option<ParsedRolloutEvents> {
        if self.is_subagent {
            return None;
        }
        let mut parsed = self.events.clone();
        parsed.pending_approvals = self.pending_approvals();
        Some(parsed)
    }
}

#[cfg(test)]
fn parse_rollout_events(contents: &str) -> Option<ParsedRolloutEvents> {
    let mut state = RolloutParseState::default();
    state.consume(contents);
    state.snapshot()
}

fn is_subagent_payload(payload: &Value) -> bool {
    payload.get("thread_source").and_then(Value::as_str) == Some("subagent")
        || payload.get("parent_thread_id").is_some_and(|parent| {
            parent
                .as_str()
                .is_some_and(|value| !value.trim().is_empty())
        })
        || payload
            .get("source")
            .and_then(Value::as_object)
            .is_some_and(|source| {
                source.contains_key("subagent") || source.contains_key("sub_agent")
            })
}

#[cfg(test)]
fn rollout_is_subagent(contents: &str) -> bool {
    parse_rollout_events(contents).is_none()
}

#[cfg(test)]
fn pending_approvals_in_rollout(contents: &str) -> Vec<(String, String)> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.pending_approvals)
        .unwrap_or_default()
}

#[cfg(test)]
fn session_lifecycle_status_in_rollout(
    contents: &str,
    _pending_approvals: &[(String, String)],
) -> SessionLifecycleStatus {
    let mut state = RolloutParseState::default();
    state.consume(contents);
    if state.is_subagent {
        return SessionLifecycleStatus::Idle;
    }
    let pending_approvals = state.pending_approvals();
    state.lifecycle_status(&pending_approvals)
}

fn task_completion_error(payload: &Value) -> Option<String> {
    payload.get("error").and_then(|error| {
        if error.is_null() {
            None
        } else {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(ToString::to_string)
                .or_else(|| Some(error.to_string()))
        }
    })
}

fn function_call_requires_approval(payload: &Value) -> bool {
    match payload.get("name").and_then(Value::as_str) {
        Some("request_permissions" | "request_user_input" | "request_user_input_async") => true,
        Some("exec_command") => {
            let Some(arguments) = payload.get("arguments") else {
                return false;
            };
            match arguments {
                Value::String(arguments) => serde_json::from_str::<Value>(arguments)
                    .ok()
                    .is_some_and(|arguments| exec_command_requires_escalation(&arguments)),
                Value::Object(_) => exec_command_requires_escalation(arguments),
                _ => false,
            }
        }
        _ => false,
    }
}

fn exec_command_requires_escalation(arguments: &Value) -> bool {
    arguments.get("sandbox_permissions").and_then(Value::as_str) == Some("require_escalated")
}

#[cfg(test)]
fn started_turns_in_rollout(contents: &str) -> Vec<String> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.started_turns)
        .unwrap_or_default()
}

#[cfg(test)]
fn completed_turns_in_rollout(contents: &str) -> Vec<(String, u128, Option<i64>, Option<String>)> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.completed_turns)
        .unwrap_or_default()
}

#[cfg(test)]
fn aborted_turns_in_rollout(contents: &str) -> Vec<String> {
    parse_rollout_events(contents)
        .map(|parsed| parsed.aborted_turns)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_subagent_rollouts_without_hiding_root_sessions() {
        let root = r#"{"type":"session_meta","payload":{"id":"root","thread_source":"codey"}}"#;
        let child = r#"{"type":"session_meta","payload":{"id":"child","thread_source":"subagent","source":{"subagent":{"thread_spawn":{"parent_thread_id":"root","depth":1}}}}}"#;
        let legacy_child =
            r#"{"type":"session_meta","payload":{"id":"child","parent_thread_id":"root"}}"#;

        assert!(!rollout_is_subagent(root));
        assert!(rollout_is_subagent(child));
        assert!(rollout_is_subagent(legacy_child));
    }

    #[test]
    fn finds_only_unresolved_waiting_calls() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_permissions","call_id":"pending"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"resolved","internal_chat_message_metadata_passthrough":{"turn_id":"turn-2"}}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"resolved"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"not-waiting"}}
{"type":"turn_context","payload":{"turn_id":"turn-3"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"aborted"}}
{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-3"}}
"#;

        assert_eq!(
            pending_approvals_in_rollout(rollout),
            vec![("turn-1".to_string(), "pending".to_string())]
        );
    }

    #[test]
    fn async_questions_remain_pending_after_acceptance_until_all_answers_arrive() {
        let mut state = RolloutParseState::default();
        state.consume(r#"
{"type":"turn_context","payload":{"turn_id":"turn-async"}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-async"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input_async","arguments":"{\"questions\":[{\"title\":\"First?\"},{\"title\":\"Second?\"}]}","call_id":"ask"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"ask","output":"{\"accepted\":true}"}}
"#);
        let pending = vec![("turn-async".to_string(), "ask".to_string())];
        assert_eq!(state.pending_approvals(), pending);
        assert_eq!(
            state.lifecycle_status(&pending),
            SessionLifecycleStatus::Waiting
        );

        let reply = |call_id: &str, index: usize| {
            let replies = serde_json::json!([{
                "questionItemId": serde_json::json!(["request_user_input_async", call_id, index]).to_string(),
                "answer": "Answer"
            }]);
            serde_json::json!({"type":"response_item","payload":{
                "type":"message","role":"user","content":[{"type":"input_text",
                "text":format!("<send_user_message_question_reply>\n{replies}\n</send_user_message_question_reply>")}]
            }})
        };
        state.apply(&reply("other", 0));
        state.apply(&reply("ask", 9));
        let mut assistant_reply = reply("ask", 1);
        assistant_reply["payload"]["role"] = Value::String("assistant".to_string());
        state.apply(&assistant_reply);
        state.consume(r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<send_user_message_question_reply>invalid</send_user_message_question_reply>"}]}}"#);
        state.apply(&reply("ask", 0));
        state.apply(&reply("ask", 0));
        assert_eq!(state.pending_approvals(), pending);
        state.apply(&reply("ask", 1));
        assert!(state.pending_approvals().is_empty());
        assert!(state.async_questions.is_empty());
        assert_eq!(state.lifecycle_status(&[]), SessionLifecycleStatus::Running);
    }

    #[test]
    fn async_questions_clear_on_failure_or_terminal_events() {
        for resolution in [
            serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"ask","output":"{\"accepted\":false}"}}),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"ask","output":"tool failed"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-async"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-async"}}),
        ] {
            let mut state = RolloutParseState::default();
            // Exercise object arguments/output and a turn ID carried by the call itself.
            state.apply(&serde_json::json!({"type":"response_item","payload":{
                "type":"function_call","name":"request_user_input_async","call_id":"ask",
                "arguments":{"questions":[{"title":"Reply?"}]},
                "internal_chat_message_metadata_passthrough":{"turn_id":"turn-async"}
            }}));
            state.apply(&serde_json::json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"ask","output":{"accepted":true}
            }}));
            assert_eq!(state.pending_approvals().len(), 1);
            state.apply(&resolution);
            assert!(state.pending_approvals().is_empty(), "{resolution}");
            assert!(state.async_questions.is_empty(), "{resolution}");
        }
    }

    #[test]
    fn finds_only_unresolved_escalated_exec_commands() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-exec"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"sandbox_permissions\":\"use_default\"}","call_id":"default"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"not-json","call_id":"invalid"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"sandbox_permissions\":\"require_escalated\"}","call_id":"resolved"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"resolved"}}
{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"sandbox_permissions\":\"require_escalated\"}","call_id":"pending"}}
"#;

        assert_eq!(
            pending_approvals_in_rollout(rollout),
            vec![("turn-exec".to_string(), "pending".to_string())]
        );
    }

    #[test]
    fn finds_authoritative_task_lifecycle_events() {
        let rollout = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","duration_ms":1234,"completed_at":200}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-error"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-error","duration_ms":500,"completed_at":300,"error":{"message":"upstream failed"}}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":""}}
{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-2","duration_ms":500}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"","duration_ms":10}}
"#;

        assert_eq!(
            started_turns_in_rollout(rollout),
            vec!["turn-1", "turn-error"]
        );
        assert_eq!(aborted_turns_in_rollout(rollout), vec!["turn-2"]);
        assert_eq!(
            completed_turns_in_rollout(rollout),
            vec![
                ("turn-1".to_string(), 1234, Some(200), None),
                (
                    "turn-error".to_string(),
                    500,
                    Some(300),
                    Some("upstream failed".to_string())
                )
            ]
        );
    }

    #[test]
    fn captures_model_and_reasoning_effort_for_each_turn() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-1","model":"gpt-5.6-luna","effort":"xhigh"}}
{"type":"turn_context","payload":{"turn_id":"turn-2","model":"gpt-5.4","effort":"low"}}
"#;

        let parsed = parse_rollout_events(rollout).unwrap();
        assert_eq!(
            parsed.turn_configurations.get("turn-1"),
            Some(&TurnConfiguration {
                model: "gpt-5.6-luna".to_string(),
                reasoning_effort: "xhigh".to_string(),
            })
        );
        assert_eq!(
            parsed.turn_configurations.get("turn-2"),
            Some(&TurnConfiguration {
                model: "gpt-5.4".to_string(),
                reasoning_effort: "low".to_string(),
            })
        );
    }

    #[test]
    fn rollout_cache_keeps_only_recent_terminal_turn_history() {
        let mut rollout = String::new();
        for index in 0..=MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT {
            rollout.push_str(&format!(
                "{{\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"turn-{index}\",\"model\":\"model-{index}\"}}}}\n"
            ));
            rollout.push_str(&format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"turn-{index}\"}}}}\n"
            ));
            rollout.push_str(&format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"turn-{index}\",\"completed_at\":{index}}}}}\n"
            ));
        }

        let mut state = RolloutParseState::default();
        state.consume(&rollout);
        let parsed = state.snapshot().unwrap();
        let latest_turn = format!("turn-{MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT}");

        assert_eq!(
            state.terminal_turn_order.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            state.terminal_turns.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            parsed.started_turns.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            parsed.completed_turns.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert_eq!(
            parsed.turn_configurations.len(),
            MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT
        );
        assert!(!state.terminal_turns.contains("turn-0"));
        assert!(!parsed.started_turns.iter().any(|turn| turn == "turn-0"));
        assert!(
            !parsed
                .completed_turns
                .iter()
                .any(|(turn, _, _, _)| turn == "turn-0")
        );
        assert!(!parsed.turn_configurations.contains_key("turn-0"));
        assert_eq!(
            parsed.started_turns.first().map(String::as_str),
            Some("turn-1")
        );
        assert_eq!(
            parsed.started_turns.last().map(String::as_str),
            Some(latest_turn.as_str())
        );
    }

    #[test]
    fn terminal_turns_release_waiting_approval_state() {
        let rollout = r#"
{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"approval-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}
"#;
        let mut state = RolloutParseState::default();

        state.consume(rollout);

        assert!(state.waiting_calls.is_empty());
        assert!(state.pending_approvals().is_empty());
    }

    #[test]
    fn derives_authoritative_session_lifecycle_status() {
        let running = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
"#;
        assert_eq!(
            session_lifecycle_status_in_rollout(running, &[]),
            SessionLifecycleStatus::Running
        );

        let waiting = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"response_item","payload":{"type":"function_call","name":"request_user_input","call_id":"approval-1"}}
"#;
        let pending = pending_approvals_in_rollout(waiting);
        assert_eq!(
            session_lifecycle_status_in_rollout(waiting, &pending),
            SessionLifecycleStatus::Waiting
        );

        let completed = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}
"#;
        assert_eq!(
            session_lifecycle_status_in_rollout(completed, &[]),
            SessionLifecycleStatus::Idle
        );

        let failed = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","error":{"message":"boom"}}}
"#;
        assert_eq!(
            session_lifecycle_status_in_rollout(failed, &[]),
            SessionLifecycleStatus::Error
        );
    }

    #[test]
    fn a_new_successful_turn_clears_an_older_error_status() {
        let rollout = r#"
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","error":{"message":"boom"}}}
{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}
{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2"}}
"#;

        assert_eq!(
            session_lifecycle_status_in_rollout(rollout, &[]),
            SessionLifecycleStatus::Idle
        );
    }
}
