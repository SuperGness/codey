use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;

/// This is the trigger name used by the public Codex workaround. Reusing it
/// lets Codey adopt an existing manual workaround and makes the off switch
/// restore writes predictably.
const BLOCK_INSERTS_TRIGGER: &str = "block_log_inserts";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TraceLogGuardReport {
    pub databases_found: usize,
    pub log_tables_found: usize,
    pub changed: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceLogCleanupReport {
    pub databases_found: usize,
    pub databases_cleaned: usize,
    pub rows_deleted: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
}

/// Enables or disables the persistent-log write guard in every Codex log
/// database that already exists. The function never creates a database: Codex
/// remains responsible for its schema and migrations.
pub fn configure(home: &Path, disable_writes: bool) -> Result<TraceLogGuardReport> {
    let mut report = TraceLogGuardReport::default();
    for path in log_database_paths(home)? {
        report.databases_found += 1;
        let outcome = configure_database(&path, disable_writes)
            .with_context(|| format!("更新 Codex Trace 日志防护失败：{}", path.display()))?;
        if outcome.table_found {
            report.log_tables_found += 1;
        }
        if outcome.changed {
            report.changed += 1;
        }
    }
    Ok(report)
}

/// Removes diagnostic rows and compacts Codex log databases without unlinking
/// files that a running app-server may still have open. Conversation/session
/// data lives elsewhere and is not touched.
pub fn clear(home: &Path) -> Result<TraceLogCleanupReport> {
    let mut report = TraceLogCleanupReport::default();
    for path in log_database_paths(home)? {
        report.databases_found += 1;
        report.bytes_before += database_family_bytes(&path);
        let rows_deleted = clear_database(&path)
            .with_context(|| format!("清理 Codex Trace 日志库失败：{}", path.display()))?;
        if let Some(rows_deleted) = rows_deleted {
            report.databases_cleaned += 1;
            report.rows_deleted += rows_deleted;
        }
        report.bytes_after += database_family_bytes(&path);
    }
    report.bytes_reclaimed = report.bytes_before.saturating_sub(report.bytes_after);
    Ok(report)
}

#[derive(Debug, Default)]
struct DatabaseOutcome {
    table_found: bool,
    changed: bool,
}

fn configure_database(path: &Path, disable_writes: bool) -> Result<DatabaseOutcome> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;

    let table_found = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='logs' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !table_found {
        return Ok(DatabaseOutcome::default());
    }

    let trigger_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1 LIMIT 1",
            [BLOCK_INSERTS_TRIGGER],
            |_| Ok(()),
        )
        .optional()?
        .is_some();

    let changed = if disable_writes && !trigger_exists {
        connection.execute_batch(
            "CREATE TRIGGER block_log_inserts
             BEFORE INSERT ON logs
             BEGIN
                 SELECT RAISE(IGNORE);
             END;",
        )?;
        true
    } else if !disable_writes && trigger_exists {
        connection.execute_batch("DROP TRIGGER block_log_inserts;")?;
        true
    } else {
        false
    };

    Ok(DatabaseOutcome {
        table_found: true,
        changed,
    })
}

fn clear_database(path: &Path) -> Result<Option<u64>> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(30))?;

    let table_found = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='logs' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !table_found {
        return Ok(None);
    }

    let rows_deleted = connection.execute("DELETE FROM logs", [])? as u64;
    checkpoint_truncate(&connection)?;
    connection.execute_batch("VACUUM;")?;
    checkpoint_truncate(&connection)?;
    Ok(Some(rows_deleted))
}

fn checkpoint_truncate(connection: &Connection) -> Result<()> {
    let _ = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    Ok(())
}

pub(crate) fn database_family_bytes(path: &Path) -> u64 {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .filter_map(|suffix| {
            let mut candidate = path.as_os_str().to_os_string();
            candidate.push(suffix);
            fs::metadata(PathBuf::from(candidate))
                .ok()
                .map(|metadata| metadata.len())
        })
        .sum()
}

pub(crate) fn log_database_paths(home: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_log_databases(home, &mut paths)?;
    collect_log_databases(&home.join("sqlite"), &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_log_databases(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 Codex 日志目录失败：{}", directory.display()));
        }
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        // A symlink commonly means the user already redirected the log DB to
        // tmpfs. Leave that workaround entirely under the user's control.
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("logs_") && name.ends_with(".sqlite") {
            paths.push(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_log_database(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    level TEXT NOT NULL
                );",
            )
            .unwrap();
    }

    #[test]
    fn enabled_guard_blocks_log_inserts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("logs_2.sqlite");
        create_log_database(&path);

        let report = configure(temp.path(), true).unwrap();

        assert_eq!(
            report,
            TraceLogGuardReport {
                databases_found: 1,
                log_tables_found: 1,
                changed: 1,
            }
        );
        let connection = Connection::open(path).unwrap();
        assert_eq!(
            connection
                .execute("INSERT INTO logs(level) VALUES ('TRACE')", [])
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn disabled_guard_restores_log_inserts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sqlite/logs_2.sqlite");
        create_log_database(&path);
        configure(temp.path(), true).unwrap();

        let report = configure(temp.path(), false).unwrap();

        assert_eq!(report.databases_found, 1);
        assert_eq!(report.log_tables_found, 1);
        assert_eq!(report.changed, 1);
        let connection = Connection::open(path).unwrap();
        assert_eq!(
            connection
                .execute("INSERT INTO logs(level) VALUES ('TRACE')", [])
                .unwrap(),
            1
        );
    }

    #[test]
    fn ignores_missing_and_non_log_databases() {
        let temp = tempfile::tempdir().unwrap();
        create_log_database(&temp.path().join("state_5.sqlite"));

        let report = configure(temp.path(), true).unwrap();

        assert_eq!(report, TraceLogGuardReport::default());
        assert!(!temp.path().join("logs_2.sqlite").exists());
    }

    #[test]
    fn updates_current_and_legacy_log_database_locations() {
        let temp = tempfile::tempdir().unwrap();
        create_log_database(&temp.path().join("logs_2.sqlite"));
        create_log_database(&temp.path().join("sqlite/logs_1.sqlite"));

        let report = configure(temp.path(), true).unwrap();

        assert_eq!(report.databases_found, 2);
        assert_eq!(report.log_tables_found, 2);
        assert_eq!(report.changed, 2);
    }

    #[test]
    fn cleanup_removes_rows_compacts_database_and_preserves_guard() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("logs_2.sqlite");
        create_log_database(&path);
        {
            let connection = Connection::open(&path).unwrap();
            let payload = "x".repeat(64 * 1024);
            for _ in 0..32 {
                connection
                    .execute("INSERT INTO logs(level) VALUES (?1)", [&payload])
                    .unwrap();
            }
        }
        configure(temp.path(), true).unwrap();

        let report = clear(temp.path()).unwrap();

        assert_eq!(report.databases_found, 1);
        assert_eq!(report.databases_cleaned, 1);
        assert_eq!(report.rows_deleted, 32);
        assert!(report.bytes_after < report.bytes_before);
        assert!(report.bytes_reclaimed > 0);
        let connection = Connection::open(path).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .execute("INSERT INTO logs(level) VALUES ('TRACE')", [])
                .unwrap(),
            0
        );
    }
}
