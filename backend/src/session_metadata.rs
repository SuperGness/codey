use std::path::Path;

use codex_plus_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

const FALLBACK_SESSION_NAME: &str = "未命名会话";
const MAX_SESSION_NAME_CHARS: usize = 80;

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
    use rusqlite::params;
    use tempfile::tempdir;

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
