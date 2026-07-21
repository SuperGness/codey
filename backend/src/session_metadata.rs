use std::collections::HashMap;
use std::path::Path;

use codex_plus_core::codex_sqlite::codex_session_db_paths_from_home;
use codex_plus_core::models::SessionRef;
use codex_plus_data::{BackupStore, SQLiteStorageAdapter};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};

const FALLBACK_SESSION_NAME: &str = "未命名会话";
const MAX_SESSION_NAME_CHARS: usize = 80;
const MAX_THREAD_SORT_KEYS: usize = 200;

pub fn thread_sort_keys(home: &Path, sessions: &[SessionRef]) -> Value {
    let sessions = sessions
        .iter()
        .filter(|session| !session.session_id.trim().is_empty())
        .take(MAX_THREAD_SORT_KEYS)
        .cloned()
        .collect::<Vec<_>>();
    if sessions.is_empty() {
        return json!({"status": "ok", "sort_keys": []});
    }

    let backup_store = BackupStore::new(home.join("backups_state/codey-thread-sort"));
    let mut latest_by_session = HashMap::<String, Value>::new();
    for path in codex_session_db_paths_from_home(home) {
        if !path.exists() {
            continue;
        }
        let result =
            SQLiteStorageAdapter::new(path, backup_store.clone()).codex_thread_sort_keys(&sessions);
        let Some(sort_keys) = result.get("sort_keys").and_then(Value::as_array) else {
            continue;
        };
        for sort_key in sort_keys {
            let Some(session_id) = sort_key.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            let should_replace = latest_by_session
                .get(session_id)
                .is_none_or(|current| timestamp_ms(sort_key) > timestamp_ms(current));
            if should_replace {
                latest_by_session.insert(session_id.to_string(), sort_key.clone());
            }
        }
    }

    let sort_keys = sessions
        .iter()
        .filter_map(|session| {
            let session_id = session
                .session_id
                .strip_prefix("local:")
                .unwrap_or(&session.session_id);
            latest_by_session.remove(session_id)
        })
        .collect::<Vec<_>>();
    json!({"status": "ok", "sort_keys": sort_keys})
}

fn timestamp_ms(payload: &Value) -> i64 {
    payload
        .get("updated_at_ms")
        .and_then(json_i64)
        .or_else(|| {
            payload
                .get("updated_at")
                .and_then(json_i64)
                .map(|seconds| seconds.saturating_mul(1_000))
        })
        .or_else(|| payload.get("created_at_ms").and_then(json_i64))
        .unwrap_or_default()
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

pub fn resolve_session_name_with_preferred(
    home: &Path,
    session_id: &str,
    preferred_title: Option<&str>,
) -> String {
    let session_id = session_id
        .trim()
        .strip_prefix("local:")
        .unwrap_or(session_id.trim());
    if session_id.is_empty() {
        return FALLBACK_SESSION_NAME.to_string();
    }

    let preferred_title = preferred_title
        .map(clean_session_name)
        .filter(|title| !title.is_empty());
    let mut found_metadata = false;
    for path in codex_session_db_paths_from_home(home) {
        if !path.exists() {
            continue;
        }
        let Ok(connection) = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        let row = connection
            .query_row(
                "SELECT title, first_user_message, preview FROM threads WHERE id=?1 LIMIT 1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional();
        let Ok(Some((title, first_user_message, preview))) = row else {
            continue;
        };
        found_metadata = true;
        let first_user_message = first_user_message
            .as_deref()
            .map(clean_session_name)
            .unwrap_or_default();
        let preview = preview
            .as_deref()
            .map(clean_session_name)
            .unwrap_or_default();
        if let Some(preferred_title) = preferred_title.as_ref()
            && !is_placeholder_title(preferred_title, &first_user_message, &preview)
        {
            return preferred_title.clone();
        }
        let title = title.as_deref().map(clean_session_name).unwrap_or_default();
        if !title.is_empty() && !is_placeholder_title(&title, &first_user_message, &preview) {
            return title;
        }
    }
    if !found_metadata && let Some(preferred_title) = preferred_title {
        return preferred_title;
    }
    FALLBACK_SESSION_NAME.to_string()
}

fn is_placeholder_title(title: &str, first_user_message: &str, preview: &str) -> bool {
    (!first_user_message.is_empty() && title == first_user_message)
        || (!preview.is_empty() && title == preview)
}

fn clean_session_name(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = normalized.chars();
    let truncated = characters
        .by_ref()
        .take(MAX_SESSION_NAME_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_plus_core::models::SessionRef;
    use rusqlite::params;
    use tempfile::tempdir;

    fn create_thread_database(path: &Path, timestamp_column: &str, rows: &[(&str, i64)]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                &format!(
                    "CREATE TABLE threads (
                        id TEXT PRIMARY KEY,
                        title TEXT NOT NULL,
                        rollout_path TEXT NOT NULL,
                        {timestamp_column} INTEGER
                    )"
                ),
                [],
            )
            .unwrap();
        for (id, timestamp) in rows {
            connection
                .execute(
                    &format!(
                        "INSERT INTO threads (id, title, rollout_path, {timestamp_column})
                         VALUES (?1, ?2, ?3, ?4)"
                    ),
                    params![
                        id,
                        format!("Title {id}"),
                        format!("/tmp/{id}.jsonl"),
                        timestamp
                    ],
                )
                .unwrap();
        }
    }

    #[test]
    fn returns_latest_thread_sort_keys_across_codex_databases() {
        let home = tempdir().unwrap();
        create_thread_database(
            &home.path().join("sqlite/codex-dev.db"),
            "updated_at_ms",
            &[("thread-1", 3_600_000), ("thread-2", 7_200_000)],
        );
        create_thread_database(
            &home.path().join("state_5.sqlite"),
            "updated_at",
            &[("thread-1", 10_800)],
        );
        let sessions = vec![
            SessionRef::new("local:thread-1", "One").unwrap(),
            SessionRef::new("thread-2", "Two").unwrap(),
            SessionRef::new("missing", "Missing").unwrap(),
        ];

        assert_eq!(
            thread_sort_keys(home.path(), &sessions),
            json!({
                "status": "ok",
                "sort_keys": [
                    {
                        "session_id": "thread-1",
                        "updated_at": 10_800,
                        "updated_at_ms": null,
                        "created_at_ms": null
                    },
                    {
                        "session_id": "thread-2",
                        "updated_at": null,
                        "updated_at_ms": 7_200_000,
                        "created_at_ms": null
                    }
                ]
            })
        );
    }

    #[test]
    fn resolves_the_saved_thread_title() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    first_user_message TEXT,
                    preview TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, title, first_user_message, preview)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    "thread-1",
                    "  发布版本计划  ",
                    "请帮我发布版本",
                    "请帮我发布版本"
                ],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "local:thread-1", None),
            "发布版本计划"
        );
    }

    #[test]
    fn never_uses_the_first_user_message_as_the_title() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    first_user_message TEXT,
                    preview TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, title, first_user_message, preview)
                 VALUES (?1, ?2, ?3, ?3)",
                params![
                    "thread-2",
                    "请帮我\n检查  飞书通知",
                    "请帮我\n检查  飞书通知"
                ],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "thread-2", None),
            FALLBACK_SESSION_NAME
        );
        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "missing", None),
            FALLBACK_SESSION_NAME
        );
    }

    #[test]
    fn prefers_the_codex_sidebar_title() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    first_user_message TEXT,
                    preview TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, title, first_user_message, preview)
                 VALUES (?1, ?2, ?2, ?2)",
                params!["thread-3", "为什么飞书标题不对"],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            resolve_session_name_with_preferred(
                home.path(),
                "local:thread-3",
                Some("  修复飞书会话标题  ")
            ),
            "修复飞书会话标题"
        );
    }

    #[test]
    fn rejects_a_sidebar_title_that_is_still_the_first_message() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    first_user_message TEXT,
                    preview TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, title, first_user_message, preview)
                 VALUES (?1, ?2, ?2, ?2)",
                params!["thread-4", "帮我处理这个问题"],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "thread-4", Some("帮我处理这个问题")),
            FALLBACK_SESSION_NAME
        );
    }
}
