//! Correlates provider identities, task paths, and transcript metadata.
//!
//! Ambiguous bindings are fenced here so lifecycle orchestration can remain
//! fail-closed without duplicating identity heuristics.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Map, Value};

use crate::subagent::lifecycle::{ExecutionOutcome, ExecutionPhase as ReservationState};
use crate::subagent::protocol;

use super::{
    AGENT_ID_COLLISION_ERROR_CODE, MAX_SPAWN_RESPONSE_JSON_BYTES,
    MAX_TRANSCRIPT_METADATA_LINE_BYTES, SessionLedger, StatusResponseSource,
    TerminalTaskObservation,
};

pub(super) fn response_is_explicit_failure(value: &Value) -> bool {
    protocol::response_is_explicit_spawn_failure(value)
        || parse_json_encoded_spawn_response(value)
            .as_ref()
            .is_some_and(protocol::response_is_explicit_spawn_failure)
}

pub(super) fn extract_agent_identifier(value: &Value) -> Option<&str> {
    protocol::extract_agent_identifier(value)
}

/// Extracts an identity that can be bound to the reservation created by the
/// current spawn call.
///
/// Newer collaboration providers return an explicit agent identifier. Some
/// providers only return the canonical task path (for example
/// `/root/review_auth`). The latter is safe to use here only because PostToolUse
/// also carries the exact input of this spawn call: the returned task path must
/// equal that task id or `/root/<task_id>`, and lookup never descends into
/// arbitrary task output fields.
pub(super) fn extract_spawn_binding_identifier(
    response: &Value,
    expected_task_id: &str,
) -> Option<String> {
    if let Some(identifier) = extract_agent_identifier(response) {
        return Some(identifier.to_string());
    }
    if let Some(task_name) = extract_matching_spawn_task_name(response, expected_task_id, true) {
        return Some(task_name.to_string());
    }
    // Direct collaboration tools currently expose their structured result to
    // PostToolUse as one JSON-encoded string. Parse only the complete, bounded
    // response and then apply the same provider-envelope checks; never scan
    // arbitrary prose or embedded output fragments for identity fields.
    let parsed = parse_json_encoded_spawn_response(response)?;
    extract_agent_identifier(&parsed)
        .or_else(|| extract_matching_spawn_task_name(&parsed, expected_task_id, true))
        .map(ToOwned::to_owned)
}

pub(super) fn parse_json_encoded_spawn_response(response: &Value) -> Option<Value> {
    let encoded = response.as_str()?.trim();
    if encoded.is_empty() || encoded.len() > MAX_SPAWN_RESPONSE_JSON_BYTES {
        return None;
    }
    serde_json::from_str(encoded).ok()
}

pub(super) fn extract_matching_spawn_task_name<'a>(
    value: &'a Value,
    expected_task_id: &str,
    provider_owned: bool,
) -> Option<&'a str> {
    match value {
        Value::Object(values) => {
            if provider_owned
                && let Some(task_name) = values.iter().find_map(|(key, value)| {
                    (normalized_identifier(key) == "taskname")
                        .then(|| value.as_str().map(str::trim))
                        .flatten()
                        .filter(|value| !value.is_empty())
                })
                && spawn_task_name_matches(task_name, expected_task_id)
            {
                return Some(task_name);
            }
            values.iter().find_map(|(key, value)| {
                protocol::is_provider_envelope_field(key)
                    .then(|| extract_matching_spawn_task_name(value, expected_task_id, true))
                    .flatten()
            })
        }
        Value::Array(values) if provider_owned => values
            .iter()
            .find_map(|value| extract_matching_spawn_task_name(value, expected_task_id, true)),
        _ => None,
    }
}

pub(super) fn spawn_task_name_matches(task_name: &str, expected_task_id: &str) -> bool {
    task_name == expected_task_id || task_name == format!("/root/{expected_task_id}")
}

pub(super) fn collect_terminal_task_outcomes(
    value: &Value,
    ledger: &mut SessionLedger,
    terminal_tasks: &mut BTreeMap<String, TerminalTaskObservation>,
    usage_by_task_turn: &mut BTreeMap<String, BTreeMap<String, crate::subagent::api::TokenUsage>>,
    source: StatusResponseSource,
    now_ms: u64,
) -> Result<bool> {
    let mut needs_snapshot = false;
    let mut observations = Vec::new();
    protocol::collect_terminal_observations(value, &mut observations);
    for observation in observations {
        let candidates = identity_task_candidates(ledger, &observation.identifier);
        if candidates.len() > 1 {
            let reason = format!(
                "终态标识 `{}` 同时指向多个 attempt（{}）",
                observation.identifier,
                candidates.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            fence_identity_conflict(ledger, &candidates, now_ms, &reason);
            anyhow::bail!("{AGENT_ID_COLLISION_ERROR_CODE}: {reason}");
        }
        let Some(task_id) = candidates.into_iter().next() else {
            continue;
        };
        let turn_hash = observation.turn_id.as_deref().map(hash_component);
        let Some(reservation) = ledger.reservations.get(&task_id) else {
            continue;
        };
        // 等待队列中的匿名终态无法区分续派轮；当前状态查询才可确认归属。
        if matches!(source, StatusResponseSource::Notification)
            && observation.turn_id.is_none()
            && reservation.followup_count > 0
        {
            needs_snapshot = true;
            continue;
        }
        let snapshot_confirms_anonymous_turn = matches!(source, StatusResponseSource::Snapshot(_))
            && turn_hash.is_none()
            && reservation.followup_turn_is_unresolved();
        if !snapshot_confirms_anonymous_turn
            && !reservation.accepts_terminal_turn(turn_hash.as_deref())
        {
            continue;
        }
        let outcome = ExecutionOutcome::from(observation.outcome);
        terminal_tasks
            .entry(task_id)
            .and_modify(|current| {
                current.outcome = stricter_outcome(current.outcome, outcome);
                current.turn_hash = current.turn_hash.clone().or_else(|| turn_hash.clone());
            })
            .or_insert(TerminalTaskObservation { outcome, turn_hash });
    }
    let mut usage_observations = Vec::new();
    protocol::collect_native_usage_observations(value, &mut usage_observations);
    for observation in usage_observations {
        let candidates = identity_task_candidates(ledger, &observation.identifier);
        if candidates.len() != 1 {
            continue;
        }
        let Some(task_id) = candidates.into_iter().next() else {
            continue;
        };
        let turn_hash = hash_component(&observation.turn_id);
        usage_by_task_turn
            .entry(task_id)
            .or_default()
            .entry(turn_hash)
            .and_modify(|current| {
                *current = super::merge_cumulative_usage(current, observation.usage.clone());
            })
            .or_insert(observation.usage);
    }
    Ok(needs_snapshot)
}

pub(super) fn stricter_outcome(
    left: ExecutionOutcome,
    right: ExecutionOutcome,
) -> ExecutionOutcome {
    if matches!(left, ExecutionOutcome::Failed | ExecutionOutcome::TimedOut)
        || matches!(right, ExecutionOutcome::Failed | ExecutionOutcome::TimedOut)
    {
        if left == ExecutionOutcome::TimedOut || right == ExecutionOutcome::TimedOut {
            ExecutionOutcome::TimedOut
        } else {
            ExecutionOutcome::Failed
        }
    } else if left == ExecutionOutcome::Lost || right == ExecutionOutcome::Lost {
        ExecutionOutcome::Lost
    } else if left == ExecutionOutcome::Succeeded || right == ExecutionOutcome::Succeeded {
        ExecutionOutcome::Succeeded
    } else {
        ExecutionOutcome::Unknown
    }
}

pub(super) fn spawn_task_id(tool_input: Option<&Value>) -> std::result::Result<Option<&str>, ()> {
    let Some(input) = tool_input.and_then(Value::as_object) else {
        return Ok(None);
    };
    consistent_string_field(input, &["task_name", "taskName"])
}

pub(super) fn followup_task_target(tool_input: Option<&Value>) -> Option<&str> {
    let input = tool_input?.as_object()?;
    string_field(input, &["target"])
        .map(str::trim)
        .filter(|target| !target.is_empty())
}

pub(super) fn interrupt_task_target(tool_input: Option<&Value>) -> Option<String> {
    match tool_input? {
        Value::Object(input) => string_field(input, &["target"])
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .map(ToOwned::to_owned),
        Value::String(encoded) if encoded.len() <= MAX_SPAWN_RESPONSE_JSON_BYTES => {
            let decoded = serde_json::from_str::<Value>(encoded).ok()?;
            interrupt_task_target(Some(&decoded))
        }
        _ => None,
    }
}

pub(super) fn string_field<'a>(values: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| values.get(*key).and_then(Value::as_str))
}

pub(super) fn consistent_string_field<'a>(
    values: &'a Map<String, Value>,
    keys: &[&str],
) -> std::result::Result<Option<&'a str>, ()> {
    let mut resolved = None;
    for key in keys {
        let Some(value) = values.get(*key) else {
            continue;
        };
        let Some(value) = value.as_str() else {
            return Err(());
        };
        if resolved.is_some_and(|existing| existing != value) {
            return Err(());
        }
        resolved = Some(value);
    }
    Ok(resolved)
}

pub(super) fn validate_unique_agent_bindings(ledger: &SessionLedger) -> Result<()> {
    let mut owners = BTreeMap::<&str, &str>::new();
    for (task_id, reservation) in &ledger.reservations {
        let Some(agent_hash) = reservation.agent_id_hash.as_deref() else {
            continue;
        };
        if let Some(existing) = owners.insert(agent_hash, task_id) {
            anyhow::bail!(
                "{AGENT_ID_COLLISION_ERROR_CODE}: Codey 子代理账本中的 agent_id 同时绑定任务 `{existing}` 与 `{task_id}`；已按 fail-closed 拒绝使用该账本"
            );
        }
    }
    Ok(())
}

pub(super) fn identity_task_candidates(
    ledger: &SessionLedger,
    identifier: &str,
) -> BTreeSet<String> {
    let identifier_hash = hash_component(identifier);
    ledger
        .reservations
        .iter()
        .filter(|(task_id, reservation)| {
            reservation.agent_id_hash.as_deref() == Some(identifier_hash.as_str())
                || reservation.last_agent_id_hash.as_deref() == Some(identifier_hash.as_str())
                || identifier_mentions_task(identifier, task_id)
        })
        .map(|(task_id, _)| task_id.clone())
        .collect()
}

/// Resolves the opaque Codex child thread id to the task path recorded in the
/// child's own rollout metadata. Codex exposes the thread id as `agent_id`, but
/// `agents.spawn_agent` returns only `/root/<task_name>`. The Hook's
/// `transcript_path` is therefore the only provider-owned object that contains
/// both values before the child's first tool call.
///
/// The transcript format is intentionally treated as a compatibility input:
/// every field and path relationship is checked, and any format drift simply
/// leaves the child unbound (fail-closed). No candidate-count or role-surface
/// heuristic is used.
pub(super) fn task_id_from_subagent_transcript(
    state_root: &Path,
    session_id: &str,
    agent_id: &str,
    agent_type: Option<&str>,
    transcript_path: Option<&str>,
    ledger: &SessionLedger,
) -> Option<String> {
    if session_id.trim().is_empty() || agent_id.trim().is_empty() {
        return None;
    }
    let canonical_transcript =
        resolve_verified_subagent_transcript(state_root, session_id, agent_id, transcript_path)?;

    let reader = BufReader::new(File::open(&canonical_transcript).ok()?);
    let mut limited = reader.take((MAX_TRANSCRIPT_METADATA_LINE_BYTES + 1) as u64);
    let mut first_line = Vec::new();
    let read = limited.read_until(b'\n', &mut first_line).ok()?;
    if read == 0 || read > MAX_TRANSCRIPT_METADATA_LINE_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&first_line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = value.get("payload")?.as_object()?;
    let nested = payload
        .get("source")
        .and_then(|value| value.get("subagent"))
        .and_then(|value| value.get("thread_spawn"))
        .and_then(Value::as_object);
    let direct_parent =
        consistent_string_field(payload, &["parent_thread_id", "parentThreadId"]).ok()?;
    let nested_parent = match nested {
        Some(values) => {
            consistent_string_field(values, &["parent_thread_id", "parentThreadId"]).ok()?
        }
        None => None,
    };
    if direct_parent.is_some() && nested_parent.is_some() && direct_parent != nested_parent {
        return None;
    }
    if consistent_string_field(payload, &["id"]).ok()? != Some(agent_id)
        || direct_parent.or(nested_parent) != Some(session_id)
    {
        return None;
    }

    let direct_path = consistent_string_field(payload, &["agent_path", "agentPath"]).ok()?;
    let nested_path = match nested {
        Some(values) => consistent_string_field(values, &["agent_path", "agentPath"]).ok()?,
        None => None,
    };
    if direct_path.is_some() && nested_path.is_some() && direct_path != nested_path {
        return None;
    }
    let agent_path = direct_path.or(nested_path)?;
    let task_id = agent_path
        .split('/')
        .rfind(|component| !component.is_empty())?;
    if agent_path != format!("/root/{task_id}") {
        return None;
    }

    let reservation = ledger.reservations.get(task_id)?;
    let active_unfenced = reservation.state.is_active() && reservation.fenced_at_ms.is_none();
    let resumable_or_provisional_terminal = reservation.resume_requested_at_ms.is_some()
        || (reservation.state == ReservationState::Terminal
            && reservation
                .agent_id_hash
                .as_deref()
                .or(reservation.last_agent_id_hash.as_deref())
                .is_some_and(|hash| is_provisional_task_binding(hash, task_id)));
    if !active_unfenced && !resumable_or_provisional_terminal {
        return None;
    }
    let direct_role = consistent_string_field(payload, &["agent_role", "agentRole"]).ok()?;
    let nested_role = match nested {
        Some(values) => consistent_string_field(values, &["agent_role", "agentRole"]).ok()?,
        None => None,
    };
    if direct_role.is_some() && nested_role.is_some() && direct_role != nested_role {
        return None;
    }
    let metadata_role = direct_role.or(nested_role)?;
    if metadata_role != reservation.role || agent_type.is_some_and(|role| role != metadata_role) {
        return None;
    }
    Some(task_id.to_string())
}

pub(crate) fn resolve_verified_subagent_transcript(
    state_root: &Path,
    session_id: &str,
    agent_id: &str,
    transcript_path: Option<&str>,
) -> Option<PathBuf> {
    let codex_home = state_root.parent()?;
    let sessions_root = fs::canonicalize(codex_home.join("sessions")).ok()?;
    let expected_suffix = format!("-{agent_id}.jsonl");
    if let Some(raw_path) = transcript_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let candidate = Path::new(raw_path);
        if !candidate.is_absolute()
            || candidate.extension().and_then(|value| value.to_str()) != Some("jsonl")
        {
            return None;
        }
        let metadata = fs::symlink_metadata(candidate).ok()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return None;
        }
        let canonical = fs::canonicalize(candidate).ok()?;
        if !canonical.starts_with(&sessions_root) {
            return None;
        }
        let file_name = canonical.file_name().and_then(|value| value.to_str())?;
        if file_name.ends_with(&expected_suffix) {
            return Some(canonical);
        }
        if !session_id.trim().is_empty() && is_parent_session_rollout_name(file_name, session_id) {
            let parent_dir = canonical.parent()?;
            return find_unique_agent_rollout_in_dir(parent_dir, &sessions_root, &expected_suffix)
                .or_else(|| find_unique_agent_rollout_in_tree(&sessions_root, &expected_suffix));
        }
        return None;
    }
    find_unique_agent_rollout_in_tree(&sessions_root, &expected_suffix)
}

fn is_parent_session_rollout_name(file_name: &str, session_id: &str) -> bool {
    let Some(stem) = file_name
        .strip_prefix("rollout-")
        .and_then(|name| name.strip_suffix(".jsonl"))
    else {
        return false;
    };
    stem.ends_with(&format!("-{session_id}")) || stem.contains(&format!("-{session_id}_"))
}

fn find_unique_agent_rollout_in_dir(
    dir: &Path,
    sessions_root: &Path,
    expected_suffix: &str,
) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut matched = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.ends_with(expected_suffix) {
            continue;
        }
        let Ok(canonical) = fs::canonicalize(&path) else {
            continue;
        };
        if !canonical.starts_with(sessions_root) {
            continue;
        }
        if matched.replace(canonical).is_some() {
            return None;
        }
    }
    matched
}

fn find_unique_agent_rollout_in_tree(
    sessions_root: &Path,
    expected_suffix: &str,
) -> Option<PathBuf> {
    let mut matched = None;
    if find_agent_rollout_in_tree_inner(
        sessions_root,
        sessions_root,
        expected_suffix,
        0,
        &mut matched,
    ) {
        matched
    } else {
        None
    }
}

fn find_agent_rollout_in_tree_inner(
    dir: &Path,
    sessions_root: &Path,
    expected_suffix: &str,
    depth: usize,
    matched: &mut Option<PathBuf>,
) -> bool {
    if depth > 4 {
        return true;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return true;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if !find_agent_rollout_in_tree_inner(
                &path,
                sessions_root,
                expected_suffix,
                depth + 1,
                matched,
            ) {
                return false;
            }
        } else if metadata.is_file()
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.ends_with(expected_suffix))
            && let Ok(canonical) = fs::canonicalize(&path)
            && canonical.starts_with(sessions_root)
            && matched.replace(canonical).is_some()
        {
            return false;
        }
    }
    true
}

fn collect_recent_session_rollout_files(
    dir: &Path,
    sessions_root: &Path,
    depth: usize,
    max_files: usize,
    target: &mut Vec<PathBuf>,
) {
    if depth > 4 || target.len() >= max_files {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for entry in entries {
        if target.len() >= max_files {
            break;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_recent_session_rollout_files(
                &path,
                sessions_root,
                depth + 1,
                max_files,
                target,
            );
        } else if metadata.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            && let Ok(canonical) = fs::canonicalize(&path)
            && canonical.starts_with(sessions_root)
        {
            target.push(canonical);
        }
    }
}

fn session_meta_agent_id(path: &Path) -> Option<String> {
    let reader = BufReader::new(File::open(path).ok()?);
    let mut limited = reader.take((MAX_TRANSCRIPT_METADATA_LINE_BYTES + 1) as u64);
    let mut first_line = Vec::new();
    let read = limited.read_until(b'\n', &mut first_line).ok()?;
    if read == 0 || read > MAX_TRANSCRIPT_METADATA_LINE_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&first_line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = value.get("payload")?.as_object()?;
    consistent_string_field(payload, &["id"])
        .ok()
        .flatten()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn rebind_provisional_reservations_from_sessions(
    state_root: &Path,
    session_id: &str,
    ledger: &mut SessionLedger,
    now_ms: u64,
) -> bool {
    let has_provisional = ledger.reservations.iter().any(|(task_id, reservation)| {
        reservation
            .agent_id_hash
            .as_deref()
            .or(reservation.last_agent_id_hash.as_deref())
            .is_some_and(|hash| is_provisional_task_binding(hash, task_id))
    });
    if !has_provisional {
        return false;
    }
    let Some(codex_home) = state_root.parent() else {
        return false;
    };
    let Ok(sessions_root) = fs::canonicalize(codex_home.join("sessions")) else {
        return false;
    };
    let mut files = Vec::new();
    collect_recent_session_rollout_files(&sessions_root, &sessions_root, 0, 64, &mut files);
    let mut changed = false;
    for file in files {
        let Some(agent_id) = session_meta_agent_id(&file) else {
            continue;
        };
        let Some(path_str) = file.to_str() else {
            continue;
        };
        let Some(task_id) = task_id_from_subagent_transcript(
            state_root,
            session_id,
            &agent_id,
            None,
            Some(path_str),
            ledger,
        ) else {
            continue;
        };
        let agent_hash = hash_component(&agent_id);
        let already_owned = ledger.reservations.iter().any(|(other_task, other)| {
            other_task != &task_id
                && (other.agent_id_hash.as_deref() == Some(agent_hash.as_str())
                    || other.last_agent_id_hash.as_deref() == Some(agent_hash.as_str()))
        });
        if already_owned {
            continue;
        }
        let Some(reservation) = ledger.reservations.get_mut(&task_id) else {
            continue;
        };
        let current_hash = reservation
            .agent_id_hash
            .as_deref()
            .or(reservation.last_agent_id_hash.as_deref());
        if current_hash.is_none_or(|hash| !is_provisional_task_binding(hash, &task_id)) {
            continue;
        }
        if reservation.state.is_active() && reservation.fenced_at_ms.is_none() {
            reservation.agent_id_hash = Some(agent_hash);
            reservation.pending_init_observed_at_ms = None;
            reservation.updated_at_ms = now_ms;
            changed = true;
        } else if reservation.state == ReservationState::Terminal {
            if reservation.agent_id_hash.is_some() {
                reservation.agent_id_hash = Some(agent_hash.clone());
            }
            reservation.last_agent_id_hash = Some(agent_hash);
            reservation.updated_at_ms = now_ms;
            changed = true;
        }
    }
    changed
}

pub(super) fn is_provisional_task_binding(bound_hash: &str, task_id: &str) -> bool {
    bound_hash == hash_component(task_id)
        || bound_hash == hash_component(&format!("/root/{task_id}"))
}

pub(super) fn unique_task_for_identifier(
    ledger: &SessionLedger,
    identifier: &str,
) -> Result<Option<String>> {
    let candidates = identity_task_candidates(ledger, identifier);
    let candidate_list = candidates.iter().cloned().collect::<Vec<_>>().join(", ");
    anyhow::ensure!(
        candidates.len() <= 1,
        "{AGENT_ID_COLLISION_ERROR_CODE}: 标识 `{identifier}` 同时指向多个 attempt（{}），拒绝猜测主体归属",
        candidate_list
    );
    Ok(candidates.into_iter().next())
}

pub(super) fn fence_identity_conflict(
    ledger: &mut SessionLedger,
    task_ids: &BTreeSet<String>,
    now_ms: u64,
    reason: &str,
) {
    for task_id in task_ids {
        let Some(reservation) = ledger.reservations.get_mut(task_id) else {
            continue;
        };
        reservation.agent_id_hash = None;
        reservation.last_agent_id_hash = None;
        reservation.pending_init_observed_at_ms = None;
        if reservation.state.is_active() {
            reservation.state = ReservationState::Recovered;
            reservation.outcome = ExecutionOutcome::Lost;
            reservation.updated_at_ms = now_ms;
            reservation.completed_at_ms = Some(now_ms);
            reservation.fenced_at_ms = Some(now_ms);
            reservation.error_message = Some(reason.to_string());
        }
    }
}

pub(super) fn identifier_mentions_task(identifier: &str, task_id: &str) -> bool {
    spawn_task_name_matches(identifier, task_id)
}

pub(super) fn normalized_identifier(value: &str) -> String {
    protocol::normalize_identifier(value)
}

pub(super) use crate::fs_util::sha256_hex_str as hash_component;

pub(super) fn hash_component_bytes(value: &[u8]) -> String {
    crate::fs_util::sha256_hex(value)
}
