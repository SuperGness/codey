//! Compatibility parsing for collaboration-tool responses.
//!
//! Tool providers have used several casing conventions and envelope shapes over
//! time.  Keeping that tolerance here gives the gate and lifecycle ledger one
//! definition of terminal state, interruption and spawn failure.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

const MAX_JSON_ENCODED_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentState {
    PendingInit,
    Live,
    Terminal,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalOutcome {
    Succeeded,
    Failed,
    TimedOut,
    Lost,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminalObservation {
    pub identifier: String,
    pub outcome: TerminalOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentStatusObservation {
    pub identifiers: Vec<String>,
    pub state: AgentState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InterruptAcknowledgement {
    /// A target-specific terminal status reported as the state observed before
    /// the interrupt. Generic `status=completed` remains a tool-call ack and
    /// therefore does not populate this field.
    pub prior_outcome: Option<TerminalOutcome>,
    /// Provider-owned identities found in the acknowledgement envelope. The
    /// orchestrator must correlate every identity with the requested target
    /// before releasing the local lifecycle fence.
    pub identifiers: Vec<String>,
}

pub(crate) fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn classify_agent_status(value: &Value) -> AgentState {
    match value {
        Value::String(value) => match normalize_identifier(value).as_str() {
            "pending" | "pendinginit" => AgentState::PendingInit,
            "running" | "live" | "interrupted" => AgentState::Live,
            value if is_terminal_value(value) => AgentState::Terminal,
            _ => AgentState::Unknown,
        },
        Value::Object(values) if object_reports_terminal(values) => AgentState::Terminal,
        _ => AgentState::Unknown,
    }
}

pub(crate) fn is_terminal_value(value: &str) -> bool {
    terminal_outcome_from_identifier(&normalize_identifier(value)).is_some()
}

pub(crate) fn value_reports_terminal(value: &Value) -> bool {
    match value {
        Value::String(value) => is_terminal_value(value),
        Value::Object(values) => object_reports_terminal(values),
        _ => false,
    }
}

pub(crate) fn object_has_terminal_status(values: &Map<String, Value>) -> bool {
    values
        .iter()
        .any(|(key, value)| is_terminal_field(key) && value_reports_terminal(value))
}

pub(crate) fn object_reports_terminal(values: &Map<String, Value>) -> bool {
    object_terminal_outcome(values).is_some()
}

pub(crate) fn collect_terminal_agent_ids(value: &Value, target: &mut Vec<String>) {
    let decoded = decode_json_encoded_response(value);
    let value = decoded.as_ref().unwrap_or(value);
    collect_terminal_identifiers_with(
        value,
        target,
        |key| normalize_identifier(key) == "agentid",
        0,
        true,
    );
}

pub(crate) fn collect_terminal_observations(value: &Value, target: &mut Vec<TerminalObservation>) {
    let decoded = decode_json_encoded_response(value);
    let value = decoded.as_ref().unwrap_or(value);
    collect_terminal_observations_in_envelope(value, target, 0, true);
}

/// Collects provider-owned per-agent lifecycle observations from list/status
/// envelopes. Each entry keeps all identities supplied by the provider so the
/// orchestrator can require them to resolve to one reservation before changing
/// a persisted PendingInit timer.
pub(crate) fn collect_agent_status_observations(
    value: &Value,
    target: &mut Vec<AgentStatusObservation>,
) {
    let decoded = decode_json_encoded_response(value);
    let value = decoded.as_ref().unwrap_or(value);
    collect_agent_status_observations_in_envelope(value, target, 0, true);
}

/// Detects a provider-owned collaboration update emitted when a child could
/// not open or did not receive the encrypted NEW_TASK payload. Codey does not
/// decrypt that payload locally; the safe recovery is one active restatement.
pub(crate) fn response_reports_task_body_unavailable(value: &Value) -> bool {
    let decoded = decode_json_encoded_response(value);
    let value = decoded.as_ref().unwrap_or(value);
    response_contains_task_body_unavailable(value, 0, true)
}

fn decode_json_encoded_response(value: &Value) -> Option<Value> {
    let encoded = value.as_str()?.trim();
    if encoded.is_empty() || encoded.len() > MAX_JSON_ENCODED_RESPONSE_BYTES {
        return None;
    }
    serde_json::from_str(encoded).ok()
}

fn response_contains_task_body_unavailable(
    value: &Value,
    depth: usize,
    entry_allowed: bool,
) -> bool {
    if depth > 8 {
        return false;
    }
    match value {
        Value::String(value) => entry_allowed && text_reports_task_body_unavailable(value),
        Value::Array(values) if entry_allowed => values
            .iter()
            .any(|value| response_contains_task_body_unavailable(value, depth + 1, true)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            let normalized_key = normalize_identifier(key);
            let text_field = matches!(
                normalized_key.as_str(),
                "message" | "text" | "content" | "output" | "reason" | "error"
            );
            (entry_allowed
                && text_field
                && nested_text_reports_task_body_unavailable(value, depth + 1))
                || ((is_agent_collection_field(key) || is_provider_envelope_field(key))
                    && response_contains_task_body_unavailable(value, depth + 1, true))
        }),
        _ => false,
    }
}

fn nested_text_reports_task_body_unavailable(value: &Value, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    match value {
        Value::String(value) => text_reports_task_body_unavailable(value),
        Value::Array(values) => values
            .iter()
            .any(|value| nested_text_reports_task_body_unavailable(value, depth + 1)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                normalize_identifier(key).as_str(),
                "message" | "text" | "content" | "output" | "reason" | "error"
            ) && nested_text_reports_task_body_unavailable(value, depth + 1)
        }),
        _ => false,
    }
}

fn text_reports_task_body_unavailable(value: &str) -> bool {
    if [
        "任务正文未能解密",
        "任务正文无法解密",
        "无法解密任务正文",
        "任务内容未能解密",
        "任务内容无法解密",
        "任务体为空",
        "任务正文为空",
        "任务内容为空",
        "任务体缺失",
        "任务正文缺失",
        "任务内容缺失",
        "任务 payload 为空",
        "任务payload为空",
        "任务 payload 缺失",
        "任务payload缺失",
    ]
    .iter()
    .any(|pattern| value.contains(pattern))
    {
        return true;
    }
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    [
        "task body could not be decrypted",
        "task body couldn't be decrypted",
        "unable to decrypt the task body",
        "unable to decrypt task body",
        "failed to decrypt the task body",
        "failed to decrypt task body",
        "task body is empty",
        "empty task body",
        "task body is missing",
        "missing task body",
        "task payload is empty",
        "task payload is missing",
        "missing task payload",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
}

fn collect_terminal_observations_in_envelope(
    value: &Value,
    target: &mut Vec<TerminalObservation>,
    depth: usize,
    entry_allowed: bool,
) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(values) if entry_allowed => {
            for value in values {
                collect_terminal_observations_in_envelope(value, target, depth + 1, true);
            }
        }
        Value::Object(values) => {
            if entry_allowed && let Some(outcome) = object_terminal_outcome(values) {
                target.extend(values.iter().filter_map(|(key, value)| {
                    matches!(
                        normalize_identifier(key).as_str(),
                        "taskname" | "agentname" | "agentid" | "subagentid"
                    )
                    .then(|| value.as_str().map(str::trim))
                    .flatten()
                    .filter(|value| !value.is_empty())
                    .map(|identifier| TerminalObservation {
                        identifier: identifier.to_owned(),
                        outcome,
                    })
                }));
            }
            for (key, value) in values {
                if is_agent_collection_field(key) || is_provider_envelope_field(key) {
                    collect_terminal_observations_in_envelope(value, target, depth + 1, true);
                }
            }
        }
        _ => {}
    }
}

fn collect_agent_status_observations_in_envelope(
    value: &Value,
    target: &mut Vec<AgentStatusObservation>,
    depth: usize,
    entry_allowed: bool,
) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(values) if entry_allowed => {
            for value in values {
                collect_agent_status_observations_in_envelope(value, target, depth + 1, true);
            }
        }
        Value::Object(values) => {
            if entry_allowed && let Some(state) = agent_entry_state(value, values) {
                let identifiers = values
                    .iter()
                    .filter_map(|(key, value)| {
                        matches!(
                            normalize_identifier(key).as_str(),
                            "taskname" | "agentname" | "agentid" | "subagentid"
                        )
                        .then(|| value.as_str().map(str::trim))
                        .flatten()
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                if !identifiers.is_empty() {
                    target.push(AgentStatusObservation { identifiers, state });
                }
            }
            for (key, value) in values {
                if is_agent_collection_field(key) || is_provider_envelope_field(key) {
                    collect_agent_status_observations_in_envelope(value, target, depth + 1, true);
                }
            }
        }
        _ => {}
    }
}

fn agent_entry_state(value: &Value, values: &Map<String, Value>) -> Option<AgentState> {
    let direct = classify_agent_status(value);
    let mut observed = (direct != AgentState::Unknown).then_some(direct);
    for (key, value) in values {
        if !matches!(
            normalize_identifier(key).as_str(),
            "agentstatus" | "status" | "state"
        ) {
            continue;
        }
        let Ok(Some((state, _))) = target_status_observation(value) else {
            continue;
        };
        if state == AgentState::Unknown {
            continue;
        }
        if observed.is_some_and(|current| current != state) {
            return None;
        }
        observed = Some(state);
    }
    observed
}

fn collect_terminal_identifiers_with(
    value: &Value,
    target: &mut Vec<String>,
    is_identifier: impl Fn(&str) -> bool + Copy,
    depth: usize,
    entry_allowed: bool,
) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(values) if entry_allowed => {
            for value in values {
                collect_terminal_identifiers_with(value, target, is_identifier, depth + 1, true);
            }
        }
        Value::Object(values) => {
            if entry_allowed && object_has_terminal_status(values) {
                target.extend(values.iter().filter_map(|(key, value)| {
                    (is_identifier(key))
                        .then(|| value.as_str().map(str::trim))
                        .flatten()
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                }));
            }
            for (key, value) in values {
                if is_agent_collection_field(key) || is_provider_envelope_field(key) {
                    collect_terminal_identifiers_with(
                        value,
                        target,
                        is_identifier,
                        depth + 1,
                        true,
                    );
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn is_agent_collection_field(key: &str) -> bool {
    matches!(
        normalize_identifier(key).as_str(),
        "agents" | "subagents" | "children" | "updates"
    )
}

pub(crate) fn is_provider_envelope_field(key: &str) -> bool {
    matches!(
        normalize_identifier(key).as_str(),
        "result" | "structuredcontent" | "data"
    )
}

pub(crate) fn extract_agent_identifier(value: &Value) -> Option<&str> {
    match value {
        Value::Object(values) => {
            if let Some(identifier) = values.iter().find_map(|(key, value)| {
                matches!(
                    normalize_identifier(key).as_str(),
                    "agentid" | "agentname" | "subagentid"
                )
                .then(|| value.as_str().map(str::trim))
                .flatten()
                .filter(|value| !value.is_empty())
            }) {
                return Some(identifier);
            }
            // Only descend through provider envelopes. A task payload may contain
            // arbitrary `task_name`/`agent_id` fields and must not be mistaken for
            // the identity returned by the spawn provider.
            values.iter().find_map(|(key, value)| {
                is_provider_envelope_field(key)
                    .then(|| extract_agent_identifier(value))
                    .flatten()
            })
        }
        Value::Array(values) => values.iter().find_map(extract_agent_identifier),
        _ => None,
    }
}

pub(crate) fn response_is_explicit_spawn_failure(value: &Value) -> bool {
    // A concrete agent identifier is authoritative. Embedded task output may
    // legitimately contain an `error` field and must not roll back the spawn.
    if extract_agent_identifier(value).is_some() {
        return false;
    }
    response_has_structured_failure(value) || response_has_textual_spawn_failure(value)
}

/// Parses only a provider-owned, structured acknowledgement that an
/// `interrupt_agent` call reached its target. A free-form message is not enough
/// to release Codey's local lifecycle fence because it may itself be an error
/// string returned by the collaboration transport.
pub(crate) fn interrupt_acknowledgement(value: &Value) -> Option<InterruptAcknowledgement> {
    let decoded = decode_json_encoded_response(value);
    let value = decoded.as_ref().unwrap_or(value);
    if interrupt_envelope_has_failure(value, 0, true) {
        return None;
    }

    let mut accumulator = InterruptAckAccumulator::default();
    collect_interrupt_acknowledgement(value, 0, true, &mut accumulator);
    if !accumulator.acknowledged || accumulator.invalid {
        return None;
    }
    Some(InterruptAcknowledgement {
        prior_outcome: accumulator.prior_outcome,
        identifiers: accumulator.identifiers.into_iter().collect(),
    })
}

#[derive(Default)]
struct InterruptAckAccumulator {
    acknowledged: bool,
    invalid: bool,
    prior_outcome: Option<TerminalOutcome>,
    identifiers: BTreeSet<String>,
}

fn collect_interrupt_acknowledgement(
    value: &Value,
    depth: usize,
    entry_allowed: bool,
    accumulator: &mut InterruptAckAccumulator,
) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(values) if entry_allowed => {
            for value in values {
                collect_interrupt_acknowledgement(value, depth + 1, true, accumulator);
            }
        }
        Value::Object(values) => {
            if entry_allowed {
                for (key, value) in values {
                    let key = normalize_identifier(key);
                    match key.as_str() {
                        "taskname" | "agentname" | "agentid" | "subagentid" => {
                            let Some(identifier) = value
                                .as_str()
                                .map(str::trim)
                                .filter(|identifier| !identifier.is_empty())
                            else {
                                accumulator.invalid = true;
                                continue;
                            };
                            accumulator.identifiers.insert(identifier.to_owned());
                        }
                        "interrupted" | "wasinterrupted" if value.as_bool() == Some(true) => {
                            accumulator.acknowledged = true;
                        }
                        // These fields describe the target's state before/after
                        // the interrupt. A terminal value is authoritative and
                        // must not later be collapsed into Recovered/Lost.
                        "previousstatus" | "agentstatus" => {
                            collect_interrupt_identities(value, depth + 1, true, accumulator);
                            match target_status_observation(value) {
                                Err(()) => accumulator.invalid = true,
                                Ok(Some((AgentState::PendingInit | AgentState::Live, _))) => {
                                    accumulator.acknowledged = true;
                                }
                                Ok(Some((AgentState::Terminal, Some(outcome)))) => {
                                    accumulator.acknowledged = true;
                                    if accumulator
                                        .prior_outcome
                                        .is_some_and(|current| current != outcome)
                                    {
                                        accumulator.invalid = true;
                                    } else {
                                        accumulator.prior_outcome = Some(outcome);
                                    }
                                }
                                Ok(Some((AgentState::Terminal, None))) => {
                                    accumulator.invalid = true;
                                }
                                Ok(Some((AgentState::Unknown, _)) | None) => {}
                            }
                        }
                        // Generic status fields can instead describe the tool
                        // call. Preserve compatible success/live acks, but an
                        // explicit failed/lost status invalidates the response.
                        "status" | "state" => {
                            collect_interrupt_identities(value, depth + 1, true, accumulator);
                            match classify_agent_status(value) {
                                AgentState::PendingInit | AgentState::Live => {
                                    accumulator.acknowledged = true;
                                }
                                AgentState::Terminal => match terminal_outcome_from_value(value) {
                                    Some(TerminalOutcome::Succeeded) => {
                                        accumulator.acknowledged = true;
                                    }
                                    Some(
                                        TerminalOutcome::Failed
                                        | TerminalOutcome::TimedOut
                                        | TerminalOutcome::Lost,
                                    ) => accumulator.invalid = true,
                                    None => {}
                                },
                                AgentState::Unknown => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
            for (key, value) in values {
                if is_provider_envelope_field(key) {
                    collect_interrupt_acknowledgement(value, depth + 1, true, accumulator);
                }
            }
        }
        Value::String(value) if entry_allowed => {
            if let Ok(decoded) = serde_json::from_str::<Value>(value) {
                collect_interrupt_acknowledgement(&decoded, depth + 1, true, accumulator);
            }
        }
        _ => {}
    }
}

fn target_status_observation(
    value: &Value,
) -> std::result::Result<Option<(AgentState, Option<TerminalOutcome>)>, ()> {
    let direct_state = classify_agent_status(value);
    if direct_state != AgentState::Unknown {
        return Ok(Some((direct_state, terminal_outcome_from_value(value))));
    }
    let Value::Object(values) = value else {
        return Ok(None);
    };
    let mut observation = None;
    for (key, value) in values {
        if !matches!(normalize_identifier(key).as_str(), "status" | "state") {
            continue;
        }
        let state = classify_agent_status(value);
        if state == AgentState::Unknown {
            continue;
        }
        let candidate = (state, terminal_outcome_from_value(value));
        if observation.is_some_and(|current| current != candidate) {
            return Err(());
        }
        observation = Some(candidate);
    }
    Ok(observation)
}

fn collect_interrupt_identities(
    value: &Value,
    depth: usize,
    entry_allowed: bool,
    accumulator: &mut InterruptAckAccumulator,
) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(values) if entry_allowed => {
            for value in values {
                collect_interrupt_identities(value, depth + 1, true, accumulator);
            }
        }
        Value::Object(values) => {
            if entry_allowed {
                for (key, value) in values {
                    if !matches!(
                        normalize_identifier(key).as_str(),
                        "taskname" | "agentname" | "agentid" | "subagentid"
                    ) {
                        continue;
                    }
                    let Some(identifier) = value
                        .as_str()
                        .map(str::trim)
                        .filter(|identifier| !identifier.is_empty())
                    else {
                        accumulator.invalid = true;
                        continue;
                    };
                    accumulator.identifiers.insert(identifier.to_owned());
                }
            }
            for (key, value) in values {
                if is_provider_envelope_field(key) {
                    collect_interrupt_identities(value, depth + 1, true, accumulator);
                }
            }
        }
        Value::String(value) if entry_allowed => {
            if let Ok(decoded) = serde_json::from_str::<Value>(value) {
                collect_interrupt_identities(&decoded, depth + 1, true, accumulator);
            }
        }
        _ => {}
    }
}

fn interrupt_envelope_has_failure(value: &Value, depth: usize, entry_allowed: bool) -> bool {
    if depth > 8 {
        return false;
    }
    match value {
        Value::Array(values) if entry_allowed => values
            .iter()
            .any(|value| interrupt_envelope_has_failure(value, depth + 1, true)),
        Value::Object(values) => {
            (entry_allowed && response_has_structured_failure(value))
                || values.iter().any(|(key, value)| {
                    (is_provider_envelope_field(key)
                        || matches!(
                            normalize_identifier(key).as_str(),
                            "previousstatus" | "agentstatus" | "status" | "state"
                        ))
                        && interrupt_envelope_has_failure(value, depth + 1, true)
                })
        }
        Value::String(value) if entry_allowed => serde_json::from_str::<Value>(value)
            .ok()
            .as_ref()
            .is_some_and(|value| interrupt_envelope_has_failure(value, depth + 1, true)),
        _ => false,
    }
}

fn response_has_structured_failure(value: &Value) -> bool {
    let Value::Object(values) = value else {
        return false;
    };
    values
        .iter()
        .any(|(key, value)| match normalize_identifier(key).as_str() {
            "iserror" => value.as_bool() == Some(true),
            "error" => value_reports_nonempty_error(value),
            _ => false,
        })
}

pub(crate) fn value_reports_nonempty_error(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::Bool(true) => true,
    }
}

fn response_has_textual_spawn_failure(value: &Value) -> bool {
    match value {
        Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                normalize_identifier(key).as_str(),
                "content" | "message" | "output" | "result" | "text"
            ) && response_has_textual_spawn_failure(value)
        }),
        Value::Array(values) => values.iter().any(response_has_textual_spawn_failure),
        Value::String(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            [
                "collab spawn failed",
                "agent spawn failed",
                "spawn agent failed",
                "spawn_agent failed",
                "failed to spawn agent",
                "failed to spawn subagent",
            ]
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
        }
        _ => false,
    }
}

fn object_terminal_outcome(values: &Map<String, Value>) -> Option<TerminalOutcome> {
    values
        .iter()
        .filter_map(|(key, value)| {
            let normalized_key = normalize_identifier(key);
            if is_terminal_marker_field(&normalized_key)
                && let Some(outcome) = terminal_outcome_from_identifier(&normalized_key)
                && !matches!(value, Value::Bool(false) | Value::Null)
            {
                return Some(outcome);
            }
            is_terminal_field(&normalized_key)
                .then(|| terminal_outcome_from_value(value))
                .flatten()
        })
        .fold(None, |current, outcome| {
            Some(match (current, outcome) {
                (Some(TerminalOutcome::Failed), _) | (_, TerminalOutcome::Failed) => {
                    TerminalOutcome::Failed
                }
                (Some(TerminalOutcome::TimedOut), _) | (_, TerminalOutcome::TimedOut) => {
                    TerminalOutcome::TimedOut
                }
                (Some(TerminalOutcome::Lost), _) | (_, TerminalOutcome::Lost) => {
                    TerminalOutcome::Lost
                }
                _ => TerminalOutcome::Succeeded,
            })
        })
}

pub(crate) fn is_terminal_marker_field(key: &str) -> bool {
    matches!(
        key,
        "finalanswer"
            | "taskcomplete"
            | "completed"
            | "aborted"
            | "cancelled"
            | "canceled"
            | "closed"
            | "stopped"
            | "errored"
            | "failed"
            | "timedout"
            | "timeout"
            | "shutdown"
            | "notfound"
    )
}

fn terminal_outcome_from_value(value: &Value) -> Option<TerminalOutcome> {
    match value {
        Value::String(value) => terminal_outcome_from_identifier(&normalize_identifier(value)),
        Value::Object(values) => object_terminal_outcome(values),
        _ => None,
    }
}

fn terminal_outcome_from_identifier(value: &str) -> Option<TerminalOutcome> {
    match value {
        "finalanswer" | "taskcomplete" | "completed" => Some(TerminalOutcome::Succeeded),
        "errored" | "error" | "failed" => Some(TerminalOutcome::Failed),
        "timedout" | "timeout" => Some(TerminalOutcome::TimedOut),
        "aborted" | "cancelled" | "canceled" | "closed" | "stopped" | "shutdown" | "notfound" => {
            Some(TerminalOutcome::Lost)
        }
        _ => None,
    }
}

fn is_terminal_field(key: &str) -> bool {
    matches!(
        normalize_identifier(key).as_str(),
        "status"
            | "state"
            | "agentstatus"
            | "type"
            | "kind"
            | "event"
            | "messagetype"
            | "messagekind"
            | "eventname"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terminal_status_is_shared_across_envelope_shapes() {
        for value in [
            json!("FINAL_ANSWER"),
            json!({"status": "task_complete"}),
            json!({"event": {"failed": true}}),
        ] {
            assert_eq!(classify_agent_status(&value), AgentState::Terminal);
        }
        assert_eq!(
            classify_agent_status(&json!("interrupted")),
            AgentState::Live
        );

        for status in ["aborted", "cancelled", "canceled", "closed", "stopped"] {
            for value in [
                json!(status),
                json!({"status": status}),
                json!({(status): true}),
            ] {
                assert_eq!(classify_agent_status(&value), AgentState::Terminal);
            }
        }
    }

    #[test]
    fn interrupt_acknowledgement_requires_a_structured_non_error_status() {
        for response in [
            json!({ "previous_status": "running" }),
            json!({ "agent_status": "interrupted" }),
            json!({ "structuredContent": { "interrupted": true } }),
            Value::String(serde_json::to_string(&json!({ "status": "completed" })).unwrap()),
        ] {
            assert!(interrupt_acknowledgement(&response).is_some(), "{response}");
        }

        for response in [
            json!({ "isError": true, "previous_status": "running" }),
            json!({ "error": "agent not found", "status": "interrupted" }),
            json!({ "status": "failed" }),
            json!({ "state": "not_found" }),
            json!({ "status": "unknown" }),
            json!("Interrupted agent /root/worker"),
            json!({ "result": { "message": "interrupt failed" } }),
        ] {
            assert!(interrupt_acknowledgement(&response).is_none(), "{response}");
        }
    }

    #[test]
    fn interrupt_acknowledgement_preserves_identity_and_authoritative_prior_outcome() {
        let response = json!({
            "result": {
                "structuredContent": {
                    "task_name": "/root/reader_a",
                    "agent_id": "agent-reader-a",
                    "previous_status": "completed"
                }
            }
        });
        assert_eq!(
            interrupt_acknowledgement(&response),
            Some(InterruptAcknowledgement {
                prior_outcome: Some(TerminalOutcome::Succeeded),
                identifiers: vec!["/root/reader_a".into(), "agent-reader-a".into()],
            })
        );

        let encoded = Value::String(
            serde_json::to_string(&json!({
                "data": {
                    "subagent_id": "agent-reader-b",
                    "agent_status": "timed_out"
                }
            }))
            .unwrap(),
        );
        assert_eq!(
            interrupt_acknowledgement(&encoded),
            Some(InterruptAcknowledgement {
                prior_outcome: Some(TerminalOutcome::TimedOut),
                identifiers: vec!["agent-reader-b".into()],
            })
        );

        assert_eq!(
            interrupt_acknowledgement(&json!({
                "result": {
                    "agent_status": {
                        "status": "running",
                        "agent_id": "agent-reader-c"
                    }
                }
            })),
            Some(InterruptAcknowledgement {
                prior_outcome: None,
                identifiers: vec!["agent-reader-c".into()],
            })
        );

        for status in ["aborted", "cancelled", "canceled", "closed", "stopped"] {
            assert_eq!(
                interrupt_acknowledgement(&json!({"previous_status": status}))
                    .unwrap()
                    .prior_outcome,
                Some(TerminalOutcome::Lost)
            );
        }
    }

    #[test]
    fn interrupt_acknowledgement_rejects_nested_failure_or_conflicting_status() {
        for response in [
            json!({
                "previous_status": "running",
                "result": { "error": "agent not found" }
            }),
            json!({
                "interrupted": true,
                "status": "failed"
            }),
            json!({
                "previous_status": "completed",
                "result": { "agent_status": "failed" }
            }),
            json!({
                "agent_id": 42,
                "previous_status": "running"
            }),
            json!({
                "agent_status": {
                    "status": "running",
                    "error": "agent not found"
                }
            }),
        ] {
            assert!(interrupt_acknowledgement(&response).is_none(), "{response}");
        }
    }

    #[test]
    fn terminal_outcomes_do_not_conflate_failure_or_loss_with_success() {
        let value = json!({"updates": [
            {"agentId": "ok", "status": "completed"},
            {"agentId": "bad", "state": "errored"},
            {"agentId": "gone", "agentStatus": "not_found"}
        ]});
        let mut observations = Vec::new();
        collect_terminal_observations(&value, &mut observations);
        assert_eq!(
            observations,
            [
                TerminalObservation {
                    identifier: "ok".to_string(),
                    outcome: TerminalOutcome::Succeeded,
                },
                TerminalObservation {
                    identifier: "bad".to_string(),
                    outcome: TerminalOutcome::Failed,
                },
                TerminalObservation {
                    identifier: "gone".to_string(),
                    outcome: TerminalOutcome::Lost,
                },
            ]
        );
        assert_eq!(
            classify_agent_status(&json!("mystery")),
            AgentState::Unknown
        );
    }

    #[test]
    fn per_agent_status_observations_preserve_identity_and_reject_conflicts() {
        let value = json!({
            "result": {
                "agents": [
                    { "agent_name": "/root", "status": "running" },
                    {
                        "task_name": "/root/reader_a",
                        "agent_id": "agent-reader-a",
                        "agent_status": "pending_init"
                    },
                    { "agent_name": "/root/reader_b", "status": "interrupted" },
                    {
                        "agent_name": "/root/conflicted",
                        "status": "running",
                        "agent_status": "completed"
                    }
                ]
            },
            "payload": {
                "agents": [
                    { "agent_name": "/root/business", "status": "pending_init" }
                ]
            }
        });
        let mut observations = Vec::new();
        collect_agent_status_observations(&value, &mut observations);
        assert_eq!(
            observations,
            [
                AgentStatusObservation {
                    identifiers: vec!["/root".into()],
                    state: AgentState::Live,
                },
                AgentStatusObservation {
                    identifiers: vec!["/root/reader_a".into(), "agent-reader-a".into()],
                    state: AgentState::PendingInit,
                },
                AgentStatusObservation {
                    identifiers: vec!["/root/reader_b".into()],
                    state: AgentState::Live,
                },
            ]
        );
    }

    #[test]
    fn terminal_identifiers_are_collected_without_false_positive() {
        let value = json!({"updates": [
            {"agentId": "done", "agentStatus": "completed"},
            {"agentId": "live", "agentStatus": "running"}
        ]});
        let mut identifiers = Vec::new();
        collect_terminal_agent_ids(&value, &mut identifiers);
        assert_eq!(identifiers, ["done"]);
    }

    #[test]
    fn json_encoded_provider_responses_preserve_terminal_identity_and_outcome() {
        let encoded = Value::String(
            serde_json::to_string(&json!({
                "updates": [
                    {"agent_id": "done", "status": "completed"},
                    {"agent_id": "bad", "status": "failed"}
                ]
            }))
            .unwrap(),
        );
        let mut observations = Vec::new();
        collect_terminal_observations(&encoded, &mut observations);
        assert_eq!(
            observations,
            [
                TerminalObservation {
                    identifier: "done".to_string(),
                    outcome: TerminalOutcome::Succeeded,
                },
                TerminalObservation {
                    identifier: "bad".to_string(),
                    outcome: TerminalOutcome::Failed,
                }
            ]
        );

        let mut identifiers = Vec::new();
        collect_terminal_agent_ids(&encoded, &mut identifiers);
        assert_eq!(identifiers, ["done", "bad"]);
    }

    #[test]
    fn unavailable_task_bodies_are_detected_only_in_collaboration_envelopes() {
        let direct = json!({
            "updates": [{
                "agent_id": "visual-a",
                "status": "MESSAGE",
                "message": "任务正文未能解密，无法开始视觉核验。"
            }]
        });
        assert!(response_reports_task_body_unavailable(&direct));

        for message in ["payload 为空，任务体缺失。", "The task body is missing."] {
            assert!(response_reports_task_body_unavailable(&json!({
                "updates": [{
                    "agent_id": "visual-a",
                    "status": "MESSAGE",
                    "message": message
                }]
            })));
        }

        let encoded = Value::String(
            serde_json::to_string(&json!({
                "result": {
                    "structuredContent": {
                        "message": "Unable to decrypt the task body; please restate it."
                    }
                }
            }))
            .unwrap(),
        );
        assert!(response_reports_task_body_unavailable(&encoded));

        assert!(!response_reports_task_body_unavailable(&json!({
            "updates": [{
                "agent_id": "visual-a",
                "status": "MESSAGE",
                "message": "The encrypted cache was refreshed successfully."
            }]
        })));
        assert!(!response_reports_task_body_unavailable(&json!({
            "updates": [{
                "agent_id": "research-a",
                "status": "MESSAGE",
                "message": "The API response has a missing payload."
            }]
        })));
        assert!(!response_reports_task_body_unavailable(&json!({
            "output": {
                "details": "任务正文未能解密"
            }
        })));
    }

    #[test]
    fn arbitrary_nested_business_payload_cannot_report_agent_terminal_state() {
        let value = json!({
            "updates": [{
                "agent_id": "live",
                "status": "running",
                "output": {
                    "agent_id": "live",
                    "status": "completed"
                },
                "details": {
                    "task_name": "unrelated",
                    "failed": true
                }
            }]
        });
        let mut observations = Vec::new();
        collect_terminal_observations(&value, &mut observations);
        assert!(observations.is_empty());

        let mut identifiers = Vec::new();
        collect_terminal_agent_ids(&value, &mut identifiers);
        assert!(identifiers.is_empty());

        let wrapped = json!({
            "result": {
                "structuredContent": {
                    "updates": [{"agent_id": "done", "status": "completed"}]
                }
            }
        });
        collect_terminal_observations(&wrapped, &mut observations);
        assert_eq!(observations[0].identifier, "done");
    }

    #[test]
    fn agent_identifier_overrides_nested_error_output() {
        assert!(!response_is_explicit_spawn_failure(
            &json!({"agent_id": "agent-1", "output": {"error": "task output"}})
        ));
        assert_eq!(
            extract_agent_identifier(&json!({"task_name": "/root/scan_auth"})),
            None
        );
        assert_eq!(
            extract_agent_identifier(&json!({"result": {"agent_id": "agent-1"}})),
            Some("agent-1")
        );
        assert!(response_is_explicit_spawn_failure(
            &json!({"isError": true, "message": "failed"})
        ));
        for empty_error in [json!(null), json!(false), json!(""), json!([]), json!({})] {
            assert!(!response_is_explicit_spawn_failure(
                &json!({"error": empty_error, "message": "accepted"})
            ));
        }
        assert!(response_is_explicit_spawn_failure(
            &json!({"error": {"code": "capacity"}})
        ));
    }

    #[test]
    fn payload_error_fields_are_not_terminal_markers() {
        assert!(!object_reports_terminal(
            json!({"agent_id": "live", "error": "task output"})
                .as_object()
                .unwrap()
        ));
        assert!(object_reports_terminal(
            json!({"agent_id": "bad", "status": "error"})
                .as_object()
                .unwrap()
        ));
        assert!(object_reports_terminal(
            json!({"agent_id": "bad", "failed": "capacity"})
                .as_object()
                .unwrap()
        ));
    }
}
