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
pub struct CompletedTurn {
    pub session_id: String,
    pub turn_id: String,
    pub duration_ms: u128,
    pub completed_at: Option<i64>,
    pub error: Option<String>,
    pub is_snapshot_replay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLifecycleStatus {
    Idle,
    Running,
    Error,
    Waiting,
}

#[derive(Debug, Default)]
pub struct RecentSessionEvents {
    pub pending_approvals: Vec<PendingApproval>,
    pub started_turns: Vec<StartedTurn>,
    pub aborted_turns: Vec<AbortedTurn>,
    pub completed_turns: Vec<CompletedTurn>,
    pub session_statuses: HashMap<String, SessionLifecycleStatus>,
}

/// Finds recent Codey sessions with an approval or user-input tool call that
/// has not yet received its matching tool result. This is intentionally based
/// on rollout data instead of the renderer: desktop approval controls are not
/// consistently exposed in the DOM.
pub fn recent_pending_approvals(home: &Path) -> Vec<PendingApproval> {
    recent_session_events(home).pending_approvals
}

pub fn recent_session_events(home: &Path) -> RecentSessionEvents {
    let recent_after = SystemTime::now()
        .checked_sub(RECENT_SESSION_WINDOW)
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let mut events = RecentSessionEvents::default();

    for (session_id, rollout_path) in recent_codey_rollouts(home, recent_after) {
        let Ok(contents) = fs::read_to_string(&rollout_path) else {
            continue;
        };
        if rollout_is_subagent(&contents) {
            continue;
        }
        let is_snapshot_replay = rollout_path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|directory| directory == "imported");
        let duration_ms = fs::metadata(&rollout_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let pending_approvals = pending_approvals_in_rollout(&contents);
        let started_turns = started_turns_in_rollout(&contents);
        let completed_turns = completed_turns_in_rollout(&contents);
        let aborted_turns = aborted_turns_in_rollout(&contents);
        events.session_statuses.insert(
            session_id.clone(),
            session_lifecycle_status_in_rollout(&contents, &pending_approvals),
        );
        events
            .pending_approvals
            .extend(
                pending_approvals
                    .into_iter()
                    .map(|(turn_id, waiting_id)| PendingApproval {
                        session_id: session_id.clone(),
                        turn_id,
                        waiting_id,
                        duration_ms,
                    }),
            );
        events
            .started_turns
            .extend(started_turns.into_iter().map(|turn_id| StartedTurn {
                session_id: session_id.clone(),
                turn_id,
            }));
        events
            .completed_turns
            .extend(completed_turns.into_iter().map(
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
            .extend(aborted_turns.into_iter().map(|turn_id| AbortedTurn {
                session_id: session_id.clone(),
                turn_id,
            }));
    }

    events
}

fn rollout_is_subagent(contents: &str) -> bool {
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record.get("type").and_then(Value::as_str) == Some("session_meta"))
        .and_then(|record| record.get("payload").cloned())
        .is_some_and(|payload| {
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
        })
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

fn pending_approvals_in_rollout(contents: &str) -> Vec<(String, String)> {
    let mut current_turn_id = String::new();
    let mut calls = HashMap::<String, String>::new();
    let mut terminal_turns = HashSet::<String>::new();

    for line in contents.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = record.get("payload") else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str) {
                current_turn_id = turn_id.to_string();
            }
            continue;
        }
        if record.get("type").and_then(Value::as_str) == Some("event_msg") {
            if matches!(
                payload.get("type").and_then(Value::as_str),
                Some("task_complete" | "turn_aborted")
            ) && let Some(turn_id) = payload.get("turn_id").and_then(Value::as_str)
                && !turn_id.trim().is_empty()
            {
                terminal_turns.insert(turn_id.trim().to_string());
            }
            continue;
        }
        if record.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }

        match payload.get("type").and_then(Value::as_str) {
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
                calls.insert(call_id.to_string(), turn_id.to_string());
            }
            Some("function_call_output") => {
                if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
                    calls.remove(call_id);
                }
            }
            _ => {}
        }
    }

    calls
        .into_iter()
        .filter(|(_, turn_id)| !turn_id.is_empty() && !terminal_turns.contains(turn_id))
        .map(|(call_id, turn_id)| (turn_id, call_id))
        .collect()
}

fn session_lifecycle_status_in_rollout(
    contents: &str,
    pending_approvals: &[(String, String)],
) -> SessionLifecycleStatus {
    let mut active_turns = HashSet::<String>::new();
    let mut latest_terminal = SessionLifecycleStatus::Idle;

    for line in contents.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let Some(payload) = record.get("payload") else {
            continue;
        };
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
            }
            Some("task_complete") => {
                active_turns.remove(turn_id);
                latest_terminal = if task_completion_error(payload).is_some() {
                    SessionLifecycleStatus::Error
                } else {
                    SessionLifecycleStatus::Idle
                };
            }
            Some("turn_aborted") => {
                active_turns.remove(turn_id);
                latest_terminal = SessionLifecycleStatus::Idle;
            }
            _ => {}
        }
    }

    if !pending_approvals.is_empty() {
        SessionLifecycleStatus::Waiting
    } else if !active_turns.is_empty() {
        SessionLifecycleStatus::Running
    } else {
        latest_terminal
    }
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

fn started_turns_in_rollout(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("event_msg"))
        .filter_map(|record| {
            let payload = record.get("payload")?;
            if payload.get("type").and_then(Value::as_str) != Some("task_started") {
                return None;
            }
            let turn_id = payload.get("turn_id").and_then(Value::as_str)?.trim();
            (!turn_id.is_empty()).then(|| turn_id.to_string())
        })
        .collect()
}

fn completed_turns_in_rollout(contents: &str) -> Vec<(String, u128, Option<i64>, Option<String>)> {
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("event_msg"))
        .filter_map(|record| {
            let payload = record.get("payload")?;
            if payload.get("type").and_then(Value::as_str) != Some("task_complete") {
                return None;
            }
            let turn_id = payload.get("turn_id").and_then(Value::as_str)?.trim();
            if turn_id.is_empty() {
                return None;
            }
            let duration_ms = payload
                .get("duration_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u128;
            let completed_at = payload.get("completed_at").and_then(Value::as_i64);
            let error = task_completion_error(payload);
            Some((turn_id.to_string(), duration_ms, completed_at, error))
        })
        .collect()
}

fn aborted_turns_in_rollout(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("event_msg"))
        .filter_map(|record| {
            let payload = record.get("payload")?;
            if payload.get("type").and_then(Value::as_str) != Some("turn_aborted") {
                return None;
            }
            let turn_id = payload.get("turn_id").and_then(Value::as_str)?.trim();
            (!turn_id.is_empty()).then(|| turn_id.to_string())
        })
        .collect()
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
