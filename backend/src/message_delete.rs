use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use codex_plus_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDeleteResult {
    pub deleted: usize,
    pub unsupported_databases: Vec<String>,
}

pub fn delete_messages(
    home: &Path,
    session_id: &str,
    message_ids: &[String],
) -> Result<MessageDeleteResult> {
    if session_id.trim().is_empty() || message_ids.is_empty() {
        anyhow::bail!("session_id 和 message_ids 不能为空");
    }
    let session_id = session_id
        .strip_prefix("local:")
        .unwrap_or(session_id)
        .trim();
    let mut result = MessageDeleteResult {
        deleted: 0,
        unsupported_databases: Vec::new(),
    };
    if let Some(rollout_path) = find_rollout_path(home, session_id)? {
        let selected = message_ids.iter().cloned().collect::<HashSet<_>>();
        result.deleted = delete_turns_from_rollout(home, &rollout_path, &selected)?;
        return Ok(result);
    }

    // Compatibility path for older Codex builds that stored individual
    // messages in SQLite instead of turn blocks in a rollout JSONL file.
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let Some(targets) = find_message_targets(&db_path)? else {
            result
                .unsupported_databases
                .push(db_path.to_string_lossy().to_string());
            continue;
        };
        let deleted = delete_from_db(&db_path, &targets, session_id, message_ids)?;
        result.deleted += deleted;
    }
    Ok(result)
}

fn find_rollout_path(home: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let connection = Connection::open(&db_path)?;
        let has_rollout_path = connection
            .query_row(
                "SELECT 1 FROM pragma_table_info('threads') WHERE name='rollout_path' LIMIT 1",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !has_rollout_path {
            continue;
        }
        let path = connection
            .query_row(
                "SELECT rollout_path FROM threads WHERE id=?1 LIMIT 1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(path) = path {
            let path = PathBuf::from(path);
            return Ok(Some(if path.is_absolute() {
                path
            } else {
                home.join(path)
            }));
        }
    }
    Ok(None)
}

fn delete_turns_from_rollout(
    home: &Path,
    rollout_path: &Path,
    selected: &HashSet<String>,
) -> Result<usize> {
    let canonical_home = home
        .canonicalize()
        .with_context(|| format!("找不到 Codex 数据目录：{}", home.display()))?;
    let canonical_rollout = rollout_path
        .canonicalize()
        .with_context(|| format!("找不到会话记录：{}", rollout_path.display()))?;
    if !canonical_rollout.starts_with(&canonical_home) {
        anyhow::bail!("会话记录不在 Codex 数据目录内，已拒绝修改");
    }

    let original = fs::read_to_string(&canonical_rollout)
        .with_context(|| format!("读取会话记录失败：{}", canonical_rollout.display()))?;
    let mut output = String::with_capacity(original.len());
    let mut removing_turn = false;
    let mut selected_turn_seen = false;
    let mut found = HashSet::new();
    for line in original.split_inclusive('\n') {
        let json_line = line.trim_end_matches(['\r', '\n']);
        if let Some(turn_id) = task_started_turn_id(json_line) {
            removing_turn = selected.contains(&turn_id);
            if removing_turn {
                selected_turn_seen = true;
                found.insert(turn_id);
            }
        }
        // A later compaction snapshot may contain the deleted turn inside its
        // encrypted summary.  It cannot be edited safely, so discard the
        // snapshot and let Codex rebuild history from the remaining rollout.
        if selected_turn_seen && is_compacted_summary(json_line) {
            continue;
        }
        if !removing_turn {
            output.push_str(line);
        }
    }
    if found.is_empty() {
        return Ok(0);
    }

    rewrite_in_place(&canonical_rollout, output.as_bytes())
        .with_context(|| format!("写回会话记录失败：{}", canonical_rollout.display()))?;
    Ok(found.len())
}

fn is_compacted_summary(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if value.get("type").and_then(Value::as_str) != Some("compacted") {
        return false;
    }
    value
        .get("payload")
        .and_then(|payload| payload.get("replacement_history"))
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("compaction"))
        })
}

fn task_started_turn_id(line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    let payload = value.get("payload")?;
    (value.get("type").and_then(Value::as_str) == Some("event_msg")
        && payload.get("type").and_then(Value::as_str) == Some("task_started"))
    .then(|| {
        payload
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
    .flatten()
}

fn rewrite_in_place(destination: &Path, contents: &[u8]) -> std::io::Result<()> {
    // Codex keeps rollout files open in append mode. Replacing the path would
    // leave that writer attached to an unlinked inode, so preserve the file
    // identity while updating its contents.
    let mut file = fs::OpenOptions::new().write(true).open(destination)?;
    file.set_len(0)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn table_columns(path: &Path, table: &str) -> Result<Vec<String>> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns)
}

#[derive(Debug, Clone)]
struct MessageTarget {
    table: &'static str,
    id_column: &'static str,
    session_column: &'static str,
}

fn find_message_targets(path: &Path) -> Result<Option<Vec<MessageTarget>>> {
    let mut targets = Vec::new();
    for table in ["messages", "thread_items", "items"] {
        let columns = table_columns(path, table)?;
        if columns.is_empty() {
            continue;
        }
        let Some(id_column) = columns
            .iter()
            .find(|column| matches!(column.as_str(), "id" | "message_id" | "item_id"))
            .map(|column| match column.as_str() {
                "message_id" => "message_id",
                "item_id" => "item_id",
                _ => "id",
            })
        else {
            continue;
        };
        let Some(session_column) = columns
            .iter()
            .find(|column| matches!(column.as_str(), "session_id" | "thread_id"))
            .map(|column| {
                if column == "thread_id" {
                    "thread_id"
                } else {
                    "session_id"
                }
            })
        else {
            continue;
        };
        targets.push(MessageTarget {
            table,
            id_column,
            session_column,
        });
    }
    Ok((!targets.is_empty()).then_some(targets))
}

fn delete_from_db(
    path: &Path,
    targets: &[MessageTarget],
    session_id: &str,
    message_ids: &[String],
) -> Result<usize> {
    let mut connection = Connection::open(path)?;
    let placeholders = std::iter::repeat_n("?", message_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut values = message_ids.to_vec();
    values.push(session_id.to_string());
    let transaction = connection.transaction()?;
    let mut deleted = 0;
    for target in targets {
        let sql = format!(
            "DELETE FROM {} WHERE {} IN ({placeholders}) AND {} = ?",
            target.table, target.id_column, target.session_column
        );
        deleted += transaction.execute(&sql, params_from_iter(values.iter()))?;
    }
    transaction.commit()?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::tempdir;

    #[test]
    fn deletes_messages_transactionally_without_a_backup() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, body TEXT NOT NULL)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO messages (id, session_id, body) VALUES (?1, ?2, ?3), (?4, ?2, ?5)",
                params!["m1", "s1", "one", "m2", "two"],
            )
            .unwrap();
        drop(connection);

        let result = delete_messages(home.path(), "s1", &["m1".into()]).unwrap();
        assert_eq!(result.deleted, 1);
        let connection = Connection::open(path).unwrap();
        let remaining: i64 = connection
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn deletes_a_current_codex_turn_from_its_rollout() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/07/16");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-test.jsonl");
        let original = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"compacted\",\"payload\":{\"replacement_history\":[{\"type\":\"message\",\"role\":\"user\",\"content\":[]},{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"old\"}]}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\"}}\n",
        );
        fs::write(&rollout, original).unwrap();
        let db = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path) VALUES (?1, ?2)",
                params!["s1", rollout.to_string_lossy().to_string()],
            )
            .unwrap();
        drop(connection);

        let mut live_writer = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
        let result = delete_messages(home.path(), "local:s1", &["t1".into()]).unwrap();
        live_writer
            .write_all(
                b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t3\"}}\n",
            )
            .unwrap();
        live_writer.sync_all().unwrap();
        assert_eq!(result.deleted, 1);
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(remaining.contains("session_meta"));
        assert!(!remaining.contains("t1"));
        assert!(!remaining.contains("cmp_1"));
        assert!(remaining.contains("t2"));
        assert!(remaining.contains("t3"));
    }

    #[test]
    fn reapplies_hard_delete_after_a_loaded_thread_flushes_stale_history() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/07/20");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-test.jsonl");
        let deleted_turn = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"remove permanently\"}]}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
        );
        let retained_turn = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\"}}\n",
        );
        fs::write(
            &rollout,
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"s1\"}}}}\n{deleted_turn}{retained_turn}"),
        )
        .unwrap();
        let db = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path) VALUES (?1, ?2)",
                params!["s1", rollout.to_string_lossy().to_string()],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            delete_messages(home.path(), "s1", &["t1".into()])
                .unwrap()
                .deleted,
            1
        );
        fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(deleted_turn.as_bytes())
            .unwrap();

        assert_eq!(
            delete_messages(home.path(), "s1", &["t1".into()])
                .unwrap()
                .deleted,
            1
        );
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(!remaining.contains("t1"));
        assert!(!remaining.contains("remove permanently"));
        assert!(remaining.contains("t2"));
    }
}
