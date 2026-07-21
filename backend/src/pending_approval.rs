use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use codex_plus_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use serde_json::Value;

const RECENT_SESSION_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RECENT_SESSIONS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub session_id: String,
    pub turn_id: String,
    pub waiting_id: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedTurn {
    pub session_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbortedTurn {
    pub session_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnConfiguration {
    pub model: String,
    pub reasoning_effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedTurn {
    pub session_id: String,
    pub turn_id: String,
    pub duration_ms: u128,
    pub completed_at: Option<i64>,
    pub error: Option<String>,
    pub is_snapshot_replay: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLifecycleStatus {
    #[default]
    Idle,
    Running,
    Error,
    Waiting,
}

#[derive(Debug, Clone, Default)]
pub struct RecentSessionEvents {
    pub pending_approvals: Vec<PendingApproval>,
    pub started_turns: Vec<StartedTurn>,
    pub aborted_turns: Vec<AbortedTurn>,
    pub completed_turns: Vec<CompletedTurn>,
    pub session_statuses: HashMap<String, SessionLifecycleStatus>,
    pub turn_configurations: HashMap<String, HashMap<String, TurnConfiguration>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RolloutSignature {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Default)]
struct ParsedRolloutEvents {
    pending_approvals: Vec<(String, String)>,
    started_turns: Vec<String>,
    aborted_turns: Vec<String>,
    completed_turns: Vec<(String, u128, Option<i64>, Option<String>)>,
    turn_configurations: HashMap<String, TurnConfiguration>,
    status: SessionLifecycleStatus,
}

#[derive(Debug, Clone)]
struct CachedRolloutEvents {
    session_id: String,
    signature: RolloutSignature,
    parsed: Option<ParsedRolloutEvents>,
}

#[derive(Debug, Default)]
pub struct RecentSessionEventCache {
    rollouts: HashMap<PathBuf, CachedRolloutEvents>,
    #[cfg(test)]
    parse_count: usize,
}

impl RecentSessionEventCache {
    /// Finds recent session lifecycle events from rollout data. Unchanged
    /// rollouts reuse their compact parsed event set across polling cycles.
    pub fn refresh(&mut self, home: &Path) -> RecentSessionEvents {
        let recent_after = SystemTime::now()
            .checked_sub(RECENT_SESSION_WINDOW)
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default();
        self.refresh_rollouts(recent_codey_rollouts(home, recent_after))
    }

    fn refresh_rollouts(&mut self, rollouts: Vec<(String, PathBuf)>) -> RecentSessionEvents {
        let mut events = RecentSessionEvents::default();
        let active_paths = rollouts
            .iter()
            .map(|(_, path)| path.clone())
            .collect::<HashSet<_>>();
        self.rollouts.retain(|path, _| active_paths.contains(path));

        for (session_id, rollout_path) in rollouts {
            let Ok(metadata) = fs::metadata(&rollout_path) else {
                self.rollouts.remove(&rollout_path);
                continue;
            };
            let signature = RolloutSignature {
                len: metadata.len(),
                modified: metadata.modified().ok(),
            };
            let cache_is_current = self.rollouts.get(&rollout_path).is_some_and(|cached| {
                cached.session_id == session_id && cached.signature == signature
            });
            if !cache_is_current {
                let Ok(contents) = fs::read_to_string(&rollout_path) else {
                    self.rollouts.remove(&rollout_path);
                    continue;
                };
                #[cfg(test)]
                {
                    self.parse_count += 1;
                }
                self.rollouts.insert(
                    rollout_path.clone(),
                    CachedRolloutEvents {
                        session_id: session_id.clone(),
                        signature: signature.clone(),
                        parsed: parse_rollout_events(&contents),
                    },
                );
            }

            let Some(parsed) = self
                .rollouts
                .get(&rollout_path)
                .and_then(|cached| cached.parsed.as_ref())
            else {
                continue;
            };
            let is_snapshot_replay = rollout_path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|directory| directory == "imported");
            let duration_ms = signature
                .modified
                .and_then(|modified| modified.elapsed().ok())
                .map(|duration| duration.as_millis())
                .unwrap_or_default();
            events
                .session_statuses
                .insert(session_id.clone(), parsed.status);
            events
                .turn_configurations
                .insert(session_id.clone(), parsed.turn_configurations.clone());
            events.pending_approvals.extend(
                parsed.pending_approvals.iter().cloned().into_iter().map(
                    |(turn_id, waiting_id)| PendingApproval {
                        session_id: session_id.clone(),
                        turn_id,
                        waiting_id,
                        duration_ms,
                    },
                ),
            );
            events
                .started_turns
                .extend(
                    parsed
                        .started_turns
                        .iter()
                        .cloned()
                        .map(|turn_id| StartedTurn {
                            session_id: session_id.clone(),
                            turn_id,
                        }),
                );
            events
                .completed_turns
                .extend(parsed.completed_turns.iter().cloned().map(
                    |(turn_id, duration_ms, completed_at, error)| CompletedTurn {
                        session_id: session_id.clone(),
                        turn_id,
                        duration_ms,
                        completed_at,
                        error,
                        is_snapshot_replay,
                    },
                ));
            events
                .aborted_turns
                .extend(
                    parsed
                        .aborted_turns
                        .iter()
                        .cloned()
                        .map(|turn_id| AbortedTurn {
                            session_id: session_id.clone(),
                            turn_id,
                        }),
                );
        }

        events
    }
}

fn parse_rollout_events(contents: &str) -> Option<ParsedRolloutEvents> {
    let mut parsed = ParsedRolloutEvents::default();
    let mut current_turn_id = String::new();
    let mut waiting_calls = HashMap::<String, String>::new();
    let mut terminal_turns = HashSet::<String>::new();
    let mut active_turns = HashSet::<String>::new();
    let mut latest_terminal = SessionLifecycleStatus::Idle;

    for line in contents.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = record.get("payload") else {
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") if is_subagent_payload(payload) => return None,
            Some("turn_context") => {
                if let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) {
                    current_turn_id = turn_id.to_string();
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
                        parsed.turn_configurations.insert(
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
                    continue;
                };
                match payload.get("type").and_then(Value::as_str) {
                    Some("task_started") => {
                        active_turns.insert(turn_id.to_string());
                        parsed.started_turns.push(turn_id.to_string());
                    }
                    Some("task_complete") => {
                        terminal_turns.insert(turn_id.to_string());
                        active_turns.remove(turn_id);
                        let error = task_completion_error(payload);
                        latest_terminal = if error.is_some() {
                            SessionLifecycleStatus::Error
                        } else {
                            SessionLifecycleStatus::Idle
                        };
                        parsed.completed_turns.push((
                            turn_id.to_string(),
                            payload
                                .get("duration_ms")
                                .and_then(Value::as_u64)
                                .unwrap_or_default() as u128,
                            payload.get("completed_at").and_then(Value::as_i64),
                            error,
                        ));
                    }
                    Some("turn_aborted") => {
                        terminal_turns.insert(turn_id.to_string());
                        active_turns.remove(turn_id);
                        latest_terminal = SessionLifecycleStatus::Idle;
                        parsed.aborted_turns.push(turn_id.to_string());
                    }
                    _ => {}
                }
            }
            Some("response_item") => match payload.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    let name = payload
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !matches!(name, "request_permissions" | "request_user_input") {
                        continue;
                    }
                    let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let turn_id = payload
                        .get("internal_chat_message_metadata_passthrough")
                        .and_then(|metadata| metadata.get("turn_id"))
                        .and_then(Value::as_str)
                        .unwrap_or(&current_turn_id);
                    waiting_calls.insert(call_id.to_string(), turn_id.to_string());
                }
                Some("function_call_output") => {
                    if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                        waiting_calls.remove(call_id);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    parsed.pending_approvals = waiting_calls
        .into_iter()
        .filter(|(_, turn_id)| !turn_id.is_empty() && !terminal_turns.contains(turn_id))
        .map(|(call_id, turn_id)| (turn_id, call_id))
        .collect();
    parsed.pending_approvals.sort();
    parsed.status = if !parsed.pending_approvals.is_empty() {
        SessionLifecycleStatus::Waiting
    } else if !active_turns.is_empty() {
        SessionLifecycleStatus::Running
    } else {
        latest_terminal
    };
    Some(parsed)
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

fn recent_codey_rollouts(home: &Path, recent_after: i64) -> Vec<(String, PathBuf)> {
    let mut rollouts = Vec::new();
    for database_path in codex_session_db_paths_from_home(home) {
        let Ok(connection) = Connection::open_with_flags(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        let Ok(mut statement) = connection.prepare(
            "SELECT id, rollout_path FROM threads \
             WHERE archived=0 AND updated_at >= ?1 \
             ORDER BY updated_at DESC LIMIT ?2",
        ) else {
            continue;
        };
        let Ok(rows) = statement.query_map(params![recent_after, MAX_RECENT_SESSIONS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            continue;
        };
        for row in rows.filter_map(Result::ok) {
            let path = PathBuf::from(&row.1);
            rollouts.push((
                row.0,
                if path.is_absolute() {
                    path
                } else {
                    home.join(path)
                },
            ));
        }
    }
    rollouts.sort();
    rollouts.dedup();
    rollouts
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
    parse_rollout_events(contents)
        .map(|parsed| parsed.status)
        .unwrap_or(SessionLifecycleStatus::Idle)
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
    use std::io::Write;

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
    fn finds_authoritative_task_lifecycle_events() {
        let rollout = r#"
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

    #[test]
    fn unchanged_rollouts_reuse_cached_lifecycle_events() {
        let temp = tempfile::tempdir().unwrap();
        let rollout_path = temp.path().join("rollout-thread-1.jsonl");
        fs::write(
            &rollout_path,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
        )
        .unwrap();
        let rollouts = || vec![("thread-1".to_string(), rollout_path.clone())];
        let mut cache = RecentSessionEventCache::default();

        let first = cache.refresh_rollouts(rollouts());
        let second = cache.refresh_rollouts(rollouts());

        assert_eq!(cache.parse_count, 1);
        assert_eq!(
            first.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Running)
        );
        assert_eq!(second.started_turns, first.started_turns);

        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&rollout_path)
                .unwrap(),
            r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-1"}}}}"#
        )
        .unwrap();
        let updated = cache.refresh_rollouts(rollouts());

        assert_eq!(cache.parse_count, 2);
        assert_eq!(
            updated.session_statuses.get("thread-1"),
            Some(&SessionLifecycleStatus::Idle)
        );
    }
}
