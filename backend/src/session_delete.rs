use std::path::Path;

use anyhow::{Result, anyhow};
use codex_plus_core::codex_sqlite::codex_session_db_paths_from_home;
use codex_plus_core::models::{DeleteResult, DeleteStatus, SessionRef};
use codex_plus_data::{BackupStore, delete_local_from_paths};

pub fn delete_session(
    home: &Path,
    backup_root: &Path,
    session_id: &str,
    title: &str,
) -> Result<DeleteResult> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        anyhow::bail!("会话 ID 不能为空");
    }
    let session = SessionRef::new(session_id, title.trim().to_string())?;
    let result = delete_local_from_paths(
        codex_session_db_paths_from_home(home),
        BackupStore::new(backup_root),
        &session,
    );
    if matches!(result.status, DeleteStatus::LocalDeleted) {
        Ok(result)
    } else {
        Err(anyhow!(result.message))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::{Connection, params};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn deletes_the_thread_and_its_rollout_with_a_backup() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/07/18");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-thread-1.jsonl");
        fs::write(&rollout, "{\"type\":\"session_meta\"}\n").unwrap();

        let db_path = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    rollout_path TEXT NOT NULL,
                    title TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, rollout_path, title) VALUES (?1, ?2, ?3)",
                params!["thread-1", rollout.to_string_lossy(), "待删除会话"],
            )
            .unwrap();
        drop(connection);

        let backup_root = home.path().join("codey-backups");
        let result =
            delete_session(home.path(), &backup_root, "local:thread-1", "待删除会话").unwrap();

        assert_eq!(result.status, DeleteStatus::LocalDeleted);
        assert!(!rollout.exists());
        assert_eq!(
            Connection::open(db_path)
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM threads WHERE id='thread-1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert!(
            result
                .backup_path
                .as_deref()
                .is_some_and(|path| Path::new(path).exists())
        );
    }

    #[test]
    fn rejects_an_empty_session_id() {
        let home = tempdir().unwrap();
        let error = delete_session(home.path(), home.path(), "  ", "").unwrap_err();
        assert!(error.to_string().contains("会话 ID"));
    }
}
