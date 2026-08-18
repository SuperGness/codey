use std::path::Path;
use std::sync::Arc;

use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::codex_config::codex_home;
use crate::commands::AppState;
use crate::message_delete::{find_rollout_path, terminal_turn_id, turn_boundary_id};
use crate::sqlite_util::table_columns;

/// Renderer-visible snapshot for a single completed turn. Timings and token
/// counts come from the local Codex session rollout so they stay available on
/// every provider route, including the official login line that never crosses
/// Codey's protocol proxy.
pub async fn token_stats_snapshot(state: &Arc<AppState>, payload: &Value) -> Value {
    let config = state.config.read().await.clone();
    if !config.show_token_stats_card {
        return unavailable("disabled", "disabled");
    }

    let turn_id = normalize_turn_id(
        payload
            .get("turnId")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let session_id = payload
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if turn_id.is_empty() || session_id.is_empty() {
        return unavailable("unavailable", "not-found");
    }

    let home = codex_home();
    let rollout_path = match find_rollout_path(&home, &session_id) {
        Ok(Some(path)) => path,
        Ok(None) => return unavailable("unavailable", "not-found"),
        Err(_) => return unavailable("unavailable", "unreadable"),
    };

    let contents = match std::fs::read_to_string(&rollout_path) {
        Ok(contents) => contents,
        Err(_) => return unavailable("unavailable", "unreadable"),
    };

    let stats = resolve_turn_stats(&contents, &turn_id);

    // Subagent turns are attributed to this parent turn by time window when the
    // parent's task_complete carries both `completed_at` (epoch seconds) and
    // `duration_ms`. Without them we fall back to a session-level total.
    let parent_window = stats.as_ref().and_then(|stats| {
        let end = stats.completed_at?;
        let start = end - (stats.duration_ms? as i64) / 1000;
        Some((start, end))
    });

    let subagent_json =
        aggregate_subagent_usage(&home, &session_id, parent_window).map(|aggregate| {
            json!({
                "inputTokens": aggregate.input_tokens,
                "outputTokens": aggregate.output_tokens,
                "totalTokens": aggregate.total_tokens,
                "count": aggregate.count,
            })
        });

    match stats {
        Some(stats) => {
            let reason_code = if stats.usage_found() {
                "ok"
            } else {
                "no-usage"
            };
            json!({
                "status": "ok",
                "reasonCode": reason_code,
                "durationMs": stats.duration_ms,
                "inputTokens": stats.input_tokens,
                "outputTokens": stats.output_tokens,
                "totalTokens": stats.total_tokens,
                "subagentStats": subagent_json,
            })
        }
        None => unavailable("unavailable", "not-found"),
    }
}

/// Resolves a turn's stats by exact id first, then falls back to the session's
/// latest completed turn. The streaming turn's DOM key is often a temporary
/// tail alias rather than the stable `history-content:turn:<id>` form.
fn resolve_turn_stats(contents: &str, turn_id: &str) -> Option<TurnStats> {
    extract_turn_stats(contents, turn_id).or_else(|| {
        crate::message_delete::last_stable_rollout_turn_id(contents)
            .and_then(|last_turn_id| extract_turn_stats(contents, &last_turn_id))
    })
}

fn unavailable(status: &str, reason_code: &str) -> Value {
    json!({
        "status": status,
        "reasonCode": reason_code,
        "durationMs": null,
        "inputTokens": null,
        "outputTokens": null,
        "totalTokens": null,
        "subagentStats": null,
    })
}

/// Reduces Codex DOM turn keys (for example `history-content:turn:<id>`) to the
/// raw turn id stored in the rollout, mirroring message_delete normalization.
fn normalize_turn_id(value: &str) -> String {
    let value = value.trim();
    value
        .rsplit_once(":turn:")
        .map(|(_, turn_id)| turn_id.trim())
        .unwrap_or(value)
        .to_string()
}

#[derive(Default)]
struct TurnStats {
    duration_ms: Option<u64>,
    completed_at: Option<i64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl TurnStats {
    fn usage_found(&self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some() || self.total_tokens.is_some()
    }
}

/// Extracts the completed turn's `task_complete.duration_ms` and any usage
/// numbers present in its rollout records. Usage fields vary across provider
/// formats, so the walk accepts the common Response and Chat Completions names
/// and lets the last occurrence win.
fn extract_turn_stats(contents: &str, target_turn_id: &str) -> Option<TurnStats> {
    let mut stats = TurnStats::default();
    let mut in_target = false;
    let mut seen = false;

    for line in contents.lines() {
        let json_line = line.trim_end_matches('\r');
        let Ok(record) = serde_json::from_str::<Value>(json_line) else {
            continue;
        };

        if let Some(turn_id) = turn_boundary_id(json_line) {
            let matches = turn_id == target_turn_id;
            in_target = matches;
            seen |= matches;
        }

        if let Some(turn_id) = terminal_turn_id(json_line) {
            if turn_id == target_turn_id {
                seen = true;
                if let Some(duration_ms) = record
                    .get("payload")
                    .and_then(|payload| payload.get("duration_ms"))
                    .and_then(Value::as_u64)
                {
                    stats.duration_ms = Some(duration_ms);
                }
                stats.completed_at = record
                    .get("payload")
                    .and_then(|payload| payload.get("completed_at"))
                    .and_then(Value::as_i64);
                collect_usage(&record, &mut stats, 0);
                break;
            }
        }

        if in_target {
            collect_usage(&record, &mut stats, 0);
        }
    }

    seen.then_some(stats)
}

fn collect_usage(value: &Value, stats: &mut TurnStats, depth: usize) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Object(map) => {
            if let Some(tokens) = first_u64(map, &["input_tokens", "prompt_tokens"]) {
                stats.input_tokens = Some(tokens);
            }
            if let Some(tokens) = first_u64(map, &["output_tokens", "completion_tokens"]) {
                stats.output_tokens = Some(tokens);
            }
            if let Some(tokens) = map.get("total_tokens").and_then(Value::as_u64) {
                stats.total_tokens = Some(tokens);
            }
            for child in map.values() {
                collect_usage(child, stats, depth + 1);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_usage(item, stats, depth + 1);
            }
        }
        _ => {}
    }
}

fn first_u64(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_u64))
}

#[derive(Default)]
struct SubagentAggregate {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    count: usize,
}

/// Enumerates the child (subagent) thread ids recorded for a parent thread via
/// the `thread_spawn_edges` edge table. Best-effort: any unreadable database is
/// skipped, and an empty list is returned when no edges are found.
fn find_child_thread_ids(home: &Path, parent_thread_id: &str) -> Vec<String> {
    let mut children = Vec::new();
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let Ok(connection) = Connection::open(&db_path) else {
            continue;
        };
        let Ok(columns) = table_columns(&connection, "thread_spawn_edges") else {
            continue;
        };
        if !columns.contains("parent_thread_id") || !columns.contains("child_thread_id") {
            continue;
        }
        let Ok(mut statement) = connection.prepare(
            "SELECT DISTINCT child_thread_id FROM thread_spawn_edges \
             WHERE parent_thread_id = ?1 AND typeof(child_thread_id) = 'text'",
        ) else {
            continue;
        };
        let rows = statement.query_map(params![parent_thread_id], |row| row.get::<_, String>(0));
        let Ok(rows) = rows else { continue };
        for row in rows.flatten() {
            let child = row.trim().to_string();
            if !child.is_empty() && !children.contains(&child) {
                children.push(child);
            }
        }
    }
    children
}

/// Sums subagent token usage belonging to a parent turn, attributed by time
/// window `(start, end)` in epoch seconds when available, or the whole session
/// when `window` is `None`. Returns `None` when nothing matches.
fn aggregate_subagent_usage(
    home: &Path,
    parent_session_id: &str,
    window: Option<(i64, i64)>,
) -> Option<SubagentAggregate> {
    let children = find_child_thread_ids(home, parent_session_id);
    let mut aggregate = SubagentAggregate::default();
    for child_id in children {
        let Some(rollout_path) = find_rollout_path(home, &child_id).ok().flatten() else {
            continue;
        };
        let Ok(contents) = std::fs::read_to_string(&rollout_path) else {
            continue;
        };
        let (input, output, total, turns) = sum_subagent_usage(&contents, window);
        aggregate.input_tokens += input;
        aggregate.output_tokens += output;
        aggregate.total_tokens += total;
        aggregate.count += turns;
    }
    (aggregate.count > 0).then_some(aggregate)
}

/// Walks a single subagent rollout and sums usage from each completed turn whose
/// `task_complete.completed_at` falls inside `window`. A `None` window keeps all
/// completed turns (session-level fallback).
fn sum_subagent_usage(contents: &str, window: Option<(i64, i64)>) -> (u64, u64, u64, usize) {
    let mut input = 0u64;
    let mut output = 0u64;
    let mut total = 0u64;
    let mut turns = 0usize;
    let mut current = TurnStats::default();
    let mut in_turn = false;

    for line in contents.lines() {
        let json_line = line.trim_end_matches('\r');
        if turn_boundary_id(json_line).is_some() {
            current = TurnStats::default();
            in_turn = true;
            continue;
        }
        if terminal_turn_id(json_line).is_some() {
            let in_window = match window {
                Some((start, end)) => serde_json::from_str::<Value>(json_line)
                    .ok()
                    .and_then(|record| {
                        record
                            .get("payload")
                            .and_then(|payload| payload.get("completed_at"))
                            .and_then(Value::as_i64)
                    })
                    .is_some_and(|completed_at| completed_at >= start && completed_at <= end),
                None => true,
            };
            if let Ok(record) = serde_json::from_str::<Value>(json_line) {
                collect_usage(&record, &mut current, 0);
            }
            if in_window && current.usage_found() {
                let turn_input = current.input_tokens.unwrap_or(0);
                let turn_output = current.output_tokens.unwrap_or(0);
                input += turn_input;
                output += turn_output;
                total += current.total_tokens.unwrap_or(turn_input + turn_output);
                turns += 1;
            }
            current = TurnStats::default();
            in_turn = false;
            continue;
        }
        if in_turn {
            if let Ok(record) = serde_json::from_str::<Value>(json_line) {
                collect_usage(&record, &mut current, 0);
            }
        }
    }

    (input, output, total, turns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(contents: &str, turn_id: &str) -> Option<TurnStats> {
        extract_turn_stats(contents, turn_id)
    }

    #[test]
    fn extracts_duration_and_usage_from_task_complete() {
        let rollout = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\",\"duration_ms\":1234,\"usage\":{\"input_tokens\":40,\"output_tokens\":90,\"total_tokens\":130}}}\n",
        );
        let result = stats(rollout, "t1").unwrap();
        assert_eq!(result.duration_ms, Some(1234));
        assert_eq!(result.input_tokens, Some(40));
        assert_eq!(result.output_tokens, Some(90));
        assert_eq!(result.total_tokens, Some(130));
    }

    #[test]
    fn reports_no_usage_when_only_duration_is_present() {
        let rollout = concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\",\"duration_ms\":500}}\n",
        );
        let result = stats(rollout, "t1").unwrap();
        assert_eq!(result.duration_ms, Some(500));
        assert!(!result.usage_found());
    }

    #[test]
    fn recognizes_chat_completions_usage_names() {
        let rollout = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":25,\"total_tokens\":35}}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\",\"duration_ms\":800}}\n",
        );
        let result = stats(rollout, "t2").unwrap();
        assert_eq!(result.input_tokens, Some(10));
        assert_eq!(result.output_tokens, Some(25));
        assert_eq!(result.total_tokens, Some(35));
    }

    #[test]
    fn aborted_turn_has_no_duration_but_can_carry_usage() {
        let rollout = concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t3\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\",\"turn_id\":\"t3\"}}\n",
        );
        let result = stats(rollout, "t3").unwrap();
        assert_eq!(result.duration_ms, None);
    }

    #[test]
    fn missing_turn_returns_none() {
        let rollout = concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\",\"duration_ms\":10}}\n",
        );
        assert!(stats(rollout, "t9").is_none());
    }

    #[test]
    fn normalize_turn_id_strips_history_content_prefix() {
        assert_eq!(normalize_turn_id("history-content:turn:abc-123"), "abc-123");
        assert_eq!(normalize_turn_id("abc-123"), "abc-123");
        assert_eq!(normalize_turn_id("  abc-123  "), "abc-123");
    }

    #[test]
    fn unresolved_tail_key_falls_back_to_latest_completed_turn() {
        let rollout = concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\",\"duration_ms\":100,\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\",\"duration_ms\":900,\"usage\":{\"input_tokens\":40,\"output_tokens\":90,\"total_tokens\":130}}}\n",
        );
        // "tail" key is not a real turn id; resolve to the latest completed t2.
        let result = resolve_turn_stats(rollout, "tail:0:local").unwrap();
        assert_eq!(result.duration_ms, Some(900));
        assert_eq!(result.input_tokens, Some(40));
        assert_eq!(result.output_tokens, Some(90));
        assert_eq!(result.total_tokens, Some(130));
    }

    #[test]
    fn sums_usage_across_completed_subagent_turns() {
        let rollout = concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"total_tokens\":30}}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\",\"usage\":{\"input_tokens\":5,\"output_tokens\":15,\"total_tokens\":20}}}\n",
        );
        let (input, output, total, turns) = sum_subagent_usage(rollout, None);
        assert_eq!(input, 15);
        assert_eq!(output, 35);
        assert_eq!(total, 50);
        assert_eq!(turns, 2);
    }

    #[test]
    fn filters_subagent_turns_to_the_parent_window() {
        let rollout = concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\",\"completed_at\":100,\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"total_tokens\":30}}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\",\"completed_at\":300,\"usage\":{\"input_tokens\":5,\"output_tokens\":15,\"total_tokens\":20}}}\n",
        );
        // Window [150, 400] includes only t2.
        let (input, output, total, turns) = sum_subagent_usage(rollout, Some((150, 400)));
        assert_eq!(input, 5);
        assert_eq!(output, 15);
        assert_eq!(total, 20);
        assert_eq!(turns, 1);
    }

    #[test]
    fn finds_child_thread_ids_from_spawn_edges() {
        let home = tempfile::tempdir().unwrap();
        let db_path = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE thread_spawn_edges \
                 (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id) \
                 VALUES ('parent','child-1'), ('parent','child-2')",
                [],
            )
            .unwrap();
        drop(connection);

        let children = find_child_thread_ids(home.path(), "parent");
        assert_eq!(children, vec!["child-1".to_string(), "child-2".to_string()]);
        assert!(find_child_thread_ids(home.path(), "other").is_empty());
    }

    #[test]
    fn aggregates_subagent_usage_across_child_rollouts() {
        let home = tempfile::tempdir().unwrap();
        let sessions_dir = home
            .path()
            .join("sessions")
            .join("2026")
            .join("08")
            .join("18");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let child1 = sessions_dir.join("child1.jsonl");
        let child2 = sessions_dir.join("child2.jsonl");
        std::fs::write(
            &child1,
            concat!(
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"c1t1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"c1t1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"total_tokens\":30}}}\n",
            ),
        )
        .unwrap();
        std::fs::write(
            &child2,
            concat!(
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"c2t1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"c2t1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":15,\"total_tokens\":20}}}\n",
            ),
        )
        .unwrap();

        let db_path = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL);
                 CREATE TABLE thread_spawn_edges \
                 (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path) VALUES ('child-1', ?1), ('child-2', ?2)",
                params![
                    child1.to_string_lossy().to_string(),
                    child2.to_string_lossy().to_string()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO thread_spawn_edges (parent_thread_id, child_thread_id) \
                 VALUES ('parent','child-1'), ('parent','child-2')",
                [],
            )
            .unwrap();
        drop(connection);

        let aggregate = aggregate_subagent_usage(home.path(), "parent", None).unwrap();
        assert_eq!(aggregate.input_tokens, 15);
        assert_eq!(aggregate.output_tokens, 35);
        assert_eq!(aggregate.total_tokens, 50);
        assert_eq!(aggregate.count, 2);
    }
}
