use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::sqlite_util::table_columns;

const FALLBACK_SESSION_NAME: &str = "未命名会话";
const MAX_SESSION_NAME_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DatabaseSignature {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    created_at: u64,
    #[cfg(not(any(unix, windows)))]
    created_at: Option<SystemTime>,
}

#[derive(Debug)]
struct CachedConnection {
    signature: DatabaseSignature,
    connection: Connection,
}

#[derive(Debug, Default)]
pub(crate) struct SessionMetadataCache {
    connections: HashMap<PathBuf, CachedConnection>,
}

/// 会话 ID 归一：允许 `local:` 前缀与两侧空白。此前 5 处各自实现，trim
/// 顺序互不一致。
pub(crate) fn normalize_session_id(value: &str) -> &str {
    let trimmed = value.trim();
    trimmed.strip_prefix("local:").unwrap_or(trimmed).trim()
}

impl SessionMetadataCache {
    pub(crate) fn resolve_session_name_with_preferred(
        &mut self,
        home: &Path,
        session_id: &str,
        preferred_title: Option<&str>,
    ) -> String {
        let session_id = normalize_session_id(session_id);
        if session_id.is_empty() {
            return FALLBACK_SESSION_NAME.to_string();
        }

        let preferred_title = preferred_title
            .map(clean_session_name)
            .filter(|title| !title.is_empty());
        let mut thread_names = Vec::new();
        let mut catalog_titles = Vec::new();
        let mut legacy_titles = Vec::new();
        let mut placeholder_titles = HashSet::new();
        for path in self.active_database_paths(home) {
            if !self.ensure_connection(&path) {
                continue;
            }
            let metadata = {
                let connection = &self
                    .connections
                    .get(&path)
                    .expect("connection was inserted above")
                    .connection;
                session_name_metadata(connection, session_id)
            };
            let metadata = match metadata {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.connections.remove(&path);
                    continue;
                }
            };

            if let Some((name, title, first_user_message, preview)) = metadata.thread {
                for placeholder in [first_user_message, preview]
                    .into_iter()
                    .filter_map(clean_optional_session_name)
                {
                    placeholder_titles.insert(placeholder);
                }
                if let Some(name) = clean_optional_session_name(name) {
                    thread_names.push(name);
                }
                if let Some(title) = clean_optional_session_name(title) {
                    legacy_titles.push(title);
                }
            }
            catalog_titles.extend(
                metadata
                    .catalog_titles
                    .into_iter()
                    .filter_map(|value| clean_session_name_candidate(&value)),
            );
        }

        if let Some(preferred_title) = preferred_title
            && !placeholder_titles.contains(&preferred_title)
        {
            return preferred_title;
        }
        if placeholder_titles.is_empty() {
            // The catalog display title can temporarily mirror the first prompt.
            // Only trust it when a thread row supplied the values needed to
            // reject that placeholder form.
            catalog_titles.clear();
        }
        thread_names
            .into_iter()
            .chain(catalog_titles)
            .chain(legacy_titles)
            .find(|title| !placeholder_titles.contains(title))
            .unwrap_or_else(|| FALLBACK_SESSION_NAME.to_string())
    }

    pub(crate) fn resolve_session_timestamps(
        &mut self,
        home: &Path,
        session_ids: &[String],
    ) -> HashMap<String, u64> {
        let session_ids = session_ids
            .iter()
            .map(|session_id| normalize_session_id(session_id).to_string())
            .filter(|session_id| !session_id.is_empty())
            .collect::<HashSet<_>>();
        if session_ids.is_empty() {
            return HashMap::new();
        }

        let mut timestamps = HashMap::new();
        for path in self.active_database_paths(home) {
            if !self.ensure_connection(&path) {
                continue;
            }
            let unresolved = session_ids
                .iter()
                .filter(|session_id| !timestamps.contains_key(*session_id))
                .cloned()
                .collect::<Vec<_>>();
            if unresolved.is_empty() {
                break;
            }
            let rows = {
                let connection = &self
                    .connections
                    .get(&path)
                    .expect("connection was inserted above")
                    .connection;
                session_timestamp_rows(connection, &unresolved)
            };
            match rows {
                Ok(rows) => timestamps.extend(rows),
                Err(_) => {
                    self.connections.remove(&path);
                }
            }
        }
        timestamps
    }

    fn active_database_paths(&mut self, home: &Path) -> Vec<PathBuf> {
        let paths = codex_session_db_paths_from_home(home)
            .into_iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        let active = paths.iter().cloned().collect::<HashSet<_>>();
        self.connections.retain(|path, _| active.contains(path));
        paths
    }

    fn ensure_connection(&mut self, path: &Path) -> bool {
        let Ok(metadata) = fs::metadata(path) else {
            self.connections.remove(path);
            return false;
        };
        let signature = database_signature(&metadata);
        let is_current = self
            .connections
            .get(path)
            .is_some_and(|cached| cached.signature == signature);
        if !is_current {
            self.connections.remove(path);
        }
        if self.connections.contains_key(path) {
            return true;
        }
        let Ok(connection) = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            return false;
        };
        if connection.busy_timeout(Duration::from_millis(250)).is_err() {
            return false;
        }
        self.connections.insert(
            path.to_path_buf(),
            CachedConnection {
                signature,
                connection,
            },
        );
        true
    }
}

fn database_signature(metadata: &fs::Metadata) -> DatabaseSignature {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        DatabaseSignature {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        DatabaseSignature {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created_at: metadata.creation_time(),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        DatabaseSignature {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created_at: metadata.created().ok(),
        }
    }
}

#[cfg(test)]
pub fn resolve_session_name_with_preferred(
    home: &Path,
    session_id: &str,
    preferred_title: Option<&str>,
) -> String {
    SessionMetadataCache::default().resolve_session_name_with_preferred(
        home,
        session_id,
        preferred_title,
    )
}

type SessionNameRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Debug, Default)]
struct SessionNameMetadata {
    thread: Option<SessionNameRow>,
    catalog_titles: Vec<String>,
}

fn session_name_metadata(connection: &Connection, session_id: &str) -> Result<SessionNameMetadata> {
    Ok(SessionNameMetadata {
        thread: session_name_row(connection, session_id)?,
        catalog_titles: catalog_session_titles(connection, session_id)?,
    })
}

fn session_name_row(connection: &Connection, session_id: &str) -> Result<Option<SessionNameRow>> {
    let columns = table_columns(connection, "threads")?;
    if !columns.contains("id") {
        return Ok(None);
    }
    let name = if columns.contains("name") {
        "name"
    } else {
        "NULL"
    };
    let title = if columns.contains("title") {
        "title"
    } else {
        "NULL"
    };
    let first_user_message = if columns.contains("first_user_message") {
        "first_user_message"
    } else {
        "NULL"
    };
    let preview = if columns.contains("preview") {
        "preview"
    } else {
        "NULL"
    };
    Ok(connection
        .query_row(
            &format!(
                "SELECT {name}, {title}, {first_user_message}, {preview} \
                 FROM threads WHERE id=?1 LIMIT 1"
            ),
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?)
}

fn catalog_session_titles(connection: &Connection, session_id: &str) -> Result<Vec<String>> {
    let columns = table_columns(connection, "local_thread_catalog")?;
    if !["thread_id", "display_title"]
        .iter()
        .all(|column| columns.contains(*column))
    {
        return Ok(Vec::new());
    }

    let host_filter = if columns.contains("host_id") {
        " AND host_id = 'local'"
    } else {
        ""
    };
    let mut ordering = Vec::new();
    if columns.contains("source_updated_at") {
        ordering.push("source_updated_at DESC");
    }
    if columns.contains("observation_sequence") {
        ordering.push("observation_sequence DESC");
    }
    let order_by = if ordering.is_empty() {
        String::new()
    } else {
        format!(" ORDER BY {}", ordering.join(", "))
    };
    let mut statement = connection.prepare(&format!(
        "SELECT display_title FROM local_thread_catalog \
         WHERE thread_id=?1{host_filter}{order_by}"
    ))?;
    let titles = statement
        .query_map(params![session_id], |row| row.get::<_, Option<String>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(titles.into_iter().flatten().collect())
}

fn session_timestamp_rows(
    connection: &Connection,
    session_ids: &[String],
) -> Result<HashMap<String, u64>> {
    let columns = table_columns(connection, "threads")?;
    if !columns.contains("id") {
        return Ok(HashMap::new());
    }
    let mut timestamp_candidates = Vec::new();
    for (column, multiplier) in [
        ("recency_at_ms", 1),
        ("recency_at", 1_000),
        ("updated_at_ms", 1),
        ("updated_at", 1_000),
        ("created_at_ms", 1),
        ("created_at", 1_000),
    ] {
        if columns.contains(column) {
            timestamp_candidates.push(format!(
                "NULLIF(CAST({column} AS INTEGER) * {multiplier}, 0)"
            ));
        }
    }
    if timestamp_candidates.is_empty() {
        return Ok(HashMap::new());
    }

    let sql = format!(
        "SELECT COALESCE({}) FROM threads WHERE id=?1 LIMIT 1",
        timestamp_candidates.join(", ")
    );
    let mut statement = connection.prepare(&sql)?;
    let mut timestamps = HashMap::new();
    for session_id in session_ids {
        let timestamp = statement
            .query_row(params![session_id], |row| row.get::<_, Option<i64>>(0))
            .optional()?
            .flatten()
            .and_then(|timestamp| u64::try_from(timestamp).ok())
            .filter(|timestamp| *timestamp > 0);
        if let Some(timestamp) = timestamp {
            timestamps.insert(session_id.clone(), timestamp);
        }
    }
    Ok(timestamps)
}

fn clean_optional_session_name(value: Option<String>) -> Option<String> {
    value.as_deref().and_then(clean_session_name_candidate)
}

fn clean_session_name_candidate(value: &str) -> Option<String> {
    let value = clean_session_name(value);
    (!value.is_empty()).then_some(value)
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
    fn resolves_the_new_codex_thread_name() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    name TEXT,
                    title TEXT,
                    first_user_message TEXT,
                    preview TEXT
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, name, title, first_user_message, preview)
                 VALUES (?1, ?2, ?3, ?3, ?3)",
                params![
                    "thread-new-name",
                    "  修复推送会话名称  ",
                    "为什么推送都是未命名会话"
                ],
            )
            .unwrap();
        drop(connection);

        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "thread-new-name", None),
            "修复推送会话名称"
        );
    }

    #[test]
    fn resolves_the_local_catalog_display_title_before_the_legacy_title() {
        let home = tempdir().unwrap();
        let state_path = home.path().join("state_5.sqlite");
        let state = Connection::open(state_path).unwrap();
        state
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    first_user_message TEXT,
                    preview TEXT
                );
                INSERT INTO threads (id, title, first_user_message, preview)
                VALUES ('thread-catalog', '检查通知', '检查通知', '检查通知');",
            )
            .unwrap();
        drop(state);

        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let catalog = Connection::open(sqlite_dir.join("codex-dev.db")).unwrap();
        catalog
            .execute_batch(
                "CREATE TABLE local_thread_catalog (
                    host_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    display_title TEXT NOT NULL,
                    source_updated_at REAL,
                    observation_sequence INTEGER
                );
                INSERT INTO local_thread_catalog VALUES
                    ('remote', 'thread-catalog', '远端旧名称', 20, 2),
                    ('local', 'thread-catalog', '修复通知标题', 10, 1);",
            )
            .unwrap();
        drop(catalog);

        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "thread-catalog", None),
            "修复通知标题"
        );
    }

    #[test]
    fn ignores_a_catalog_title_without_thread_placeholder_metadata() {
        let home = tempdir().unwrap();
        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let catalog = Connection::open(sqlite_dir.join("codex-dev.db")).unwrap();
        catalog
            .execute_batch(
                "CREATE TABLE local_thread_catalog (
                    host_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    display_title TEXT NOT NULL
                );
                INSERT INTO local_thread_catalog VALUES
                    ('local', 'thread-catalog-only', '可能仍是首条消息');",
            )
            .unwrap();
        drop(catalog);

        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "thread-catalog-only", None),
            FALLBACK_SESSION_NAME
        );
    }

    #[test]
    fn rejects_new_name_sources_that_still_contain_the_first_message() {
        let home = tempdir().unwrap();
        let state_path = home.path().join("state_5.sqlite");
        let state = Connection::open(state_path).unwrap();
        state
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    name TEXT,
                    title TEXT,
                    first_user_message TEXT,
                    preview TEXT
                );
                INSERT INTO threads (id, name, title, first_user_message, preview)
                VALUES ('thread-placeholder', '帮我处理这个问题', '帮我处理这个问题',
                        '帮我处理这个问题', '帮我处理这个问题');",
            )
            .unwrap();
        drop(state);

        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let catalog = Connection::open(sqlite_dir.join("codex-dev.db")).unwrap();
        catalog
            .execute_batch(
                "CREATE TABLE local_thread_catalog (
                    thread_id TEXT NOT NULL,
                    display_title TEXT NOT NULL
                );
                INSERT INTO local_thread_catalog VALUES
                    ('thread-placeholder', '帮我处理这个问题');",
            )
            .unwrap();
        drop(catalog);

        assert_eq!(
            resolve_session_name_with_preferred(home.path(), "thread-placeholder", None),
            FALLBACK_SESSION_NAME
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

    #[test]
    fn resolves_visible_thread_timestamps_in_one_cached_read() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    updated_at_ms INTEGER,
                    recency_at INTEGER NOT NULL DEFAULT 0,
                    recency_at_ms INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO threads VALUES
                    ('thread-recency-ms', 10, 20, 20000, 30, 30001),
                    ('thread-recency', 11, 21, 21000, 31, 0),
                    ('thread-updated-ms', 12, 22, 22001, 0, 0),
                    ('thread-updated', 13, 23, NULL, 0, 0);",
            )
            .unwrap();
        drop(connection);

        let mut cache = SessionMetadataCache::default();
        let timestamps = cache.resolve_session_timestamps(
            home.path(),
            &[
                "local:thread-recency-ms".to_string(),
                "thread-recency".to_string(),
                "thread-updated-ms".to_string(),
                "thread-updated".to_string(),
                "missing".to_string(),
            ],
        );

        assert_eq!(timestamps["thread-recency-ms"], 30_001);
        assert_eq!(timestamps["thread-recency"], 31_000);
        assert_eq!(timestamps["thread-updated-ms"], 22_001);
        assert_eq!(timestamps["thread-updated"], 23_000);
        assert!(!timestamps.contains_key("missing"));
    }

    #[test]
    fn supports_legacy_thread_timestamp_columns() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                INSERT INTO threads VALUES ('legacy-thread', 14, 24);",
            )
            .unwrap();
        drop(connection);

        let timestamps = SessionMetadataCache::default()
            .resolve_session_timestamps(home.path(), &["legacy-thread".to_string()]);

        assert_eq!(timestamps["legacy-thread"], 24_000);
    }
}
