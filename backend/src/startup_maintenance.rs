use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use codex_plus_data::{ProviderSyncResult, ProviderSyncStatus};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::default_config_path;

const MARKER_VERSION: u32 = 1;
const MARKER_FILE: &str = "provider-sync-marker-v1.json";
const PROVIDER_SYNC_MANAGED_BY: &str = "Codex++ provider sync";
const SESSION_DIRS: [&str; 2] = ["sessions", "archived_sessions"];
const MAX_ROLLOUT_HEADER_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSyncPlan {
    Full,
    Cached,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderSyncMarker {
    version: u32,
    target_provider: String,
    validated_at_ms: u128,
}

pub fn provider_sync_plan(home: &Path, target_provider: &str) -> Result<ProviderSyncPlan> {
    provider_sync_plan_at(home, target_provider, &marker_path())
}

pub fn record_provider_sync_success(target_provider: &str) -> Result<()> {
    write_marker(&marker_path(), target_provider)
}

pub fn cached_provider_sync_result(target_provider: &str) -> ProviderSyncResult {
    ProviderSyncResult {
        status: ProviderSyncStatus::Synced,
        message: "Provider sync cache is valid".to_string(),
        target_provider: target_provider.to_string(),
        backup_dir: None,
        changed_session_files: 0,
        skipped_locked_rollout_files: Vec::new(),
        sqlite_rows_updated: 0,
        sqlite_provider_rows_updated: 0,
        sqlite_user_event_rows_updated: 0,
        sqlite_cwd_rows_updated: 0,
        updated_workspace_roots: 0,
        encrypted_content_warning: None,
    }
}

fn marker_path() -> PathBuf {
    default_config_path()
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join(MARKER_FILE)
}

fn provider_sync_plan_at(
    home: &Path,
    target_provider: &str,
    marker: &Path,
) -> Result<ProviderSyncPlan> {
    let marker_matches = read_marker(marker).is_some_and(|saved| {
        saved.version == MARKER_VERSION && saved.target_provider == target_provider
    });
    let previous_sync_matches =
        marker_matches || has_legacy_provider_sync(home, target_provider).unwrap_or(false);
    if !previous_sync_matches || !provider_state_matches(home, target_provider)? {
        return Ok(ProviderSyncPlan::Full);
    }
    if !marker_matches {
        write_marker(marker, target_provider)?;
    }
    Ok(ProviderSyncPlan::Cached)
}

fn read_marker(path: &Path) -> Option<ProviderSyncMarker> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_marker(path: &Path, target_provider: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Provider 同步标记路径没有父目录"))?;
    fs::create_dir_all(parent)?;
    let marker = ProviderSyncMarker {
        version: MARKER_VERSION,
        target_provider: target_provider.to_string(),
        validated_at_ms: timestamp_millis(),
    };
    let temp = parent.join(format!(
        ".{MARKER_FILE}.{}-{}.tmp",
        std::process::id(),
        timestamp_millis()
    ));
    fs::write(&temp, serde_json::to_vec_pretty(&marker)?)?;
    if let Err(error) = fs::rename(&temp, path) {
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)?;
            fs::rename(&temp, path)?;
            return Ok(());
        }
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

fn has_legacy_provider_sync(home: &Path, target_provider: &str) -> Result<bool> {
    let root = home.join("backups_state/provider-sync");
    if !root.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(bytes) = fs::read(path.join("metadata.json")) else {
            continue;
        };
        let Ok(metadata) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if metadata.get("managedBy").and_then(Value::as_str) == Some(PROVIDER_SYNC_MANAGED_BY)
            && metadata.get("targetProvider").and_then(Value::as_str) == Some(target_provider)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn provider_state_matches(home: &Path, target_provider: &str) -> Result<bool> {
    if !rollout_headers_match(home, target_provider)? {
        return Ok(false);
    }
    sqlite_providers_match(home, target_provider)
}

fn rollout_headers_match(home: &Path, target_provider: &str) -> Result<bool> {
    for path in rollout_files(home)? {
        let file =
            fs::File::open(&path).with_context(|| format!("读取会话头失败：{}", path.display()))?;
        let reader = BufReader::new(file).take(MAX_ROLLOUT_HEADER_BYTES);
        let mut found_session_meta = false;
        for line in reader.lines() {
            let line = line?;
            if !line.contains("session_meta") {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if record.get("type").and_then(Value::as_str) != Some("session_meta") {
                continue;
            }
            found_session_meta = true;
            let provider = record
                .pointer("/payload/model_provider")
                .and_then(Value::as_str);
            if provider != Some(target_provider) {
                return Ok(false);
            }
        }
        if !found_session_meta {
            return Ok(false);
        }
    }
    Ok(true)
}

fn rollout_files(home: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for dirname in SESSION_DIRS {
        let root = home.join(dirname);
        if root.exists() {
            collect_rollout_files(&root, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn collect_rollout_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        fs::read_dir(root).with_context(|| format!("扫描会话目录失败：{}", root.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            collect_rollout_files(&path, files)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn sqlite_providers_match(home: &Path, target_provider: &str) -> Result<bool> {
    for path in codex_plus_core::codex_sqlite::codex_session_db_paths_from_home(home) {
        if !path.exists() {
            continue;
        }
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("检查 Codex Provider 数据库失败：{}", path.display()))?;
        connection.busy_timeout(Duration::from_millis(250))?;
        if !table_columns(&connection, "threads")?.contains("model_provider") {
            continue;
        }
        let mismatch = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM threads
                WHERE COALESCE(model_provider, '') <> ?1
                LIMIT 1
            )",
            [target_provider],
            |row| row.get::<_, bool>(0),
        )?;
        if mismatch {
            return Ok(false);
        }
    }
    Ok(true)
}

fn table_columns(db: &Connection, table: &str) -> Result<HashSet<String>> {
    let escaped = table.replace('"', "\"\"");
    let mut statement = db.prepare(&format!("PRAGMA table_info(\"{escaped}\")"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?)
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_rollout(home: &Path, name: &str, provider: &str) {
        let sessions = home.join("sessions/2026/07/20");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join(name),
            format!(
                "{}\n{}\n",
                json!({
                    "type": "session_meta",
                    "payload": {"id": "thread-1", "model_provider": provider}
                }),
                json!({"type": "response_item", "payload": "history"})
            ),
        )
        .unwrap();
    }

    fn write_legacy_sync(home: &Path, provider: &str) {
        let backup = home.join("backups_state/provider-sync/20260720180444");
        fs::create_dir_all(&backup).unwrap();
        fs::write(
            backup.join("metadata.json"),
            serde_json::to_vec(&json!({
                "managedBy": PROVIDER_SYNC_MANAGED_BY,
                "targetProvider": provider,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn first_run_without_previous_sync_requires_full_maintenance() {
        let temp = tempfile::tempdir().unwrap();
        write_rollout(temp.path(), "rollout-thread-1.jsonl", "codey_global");
        let marker = temp.path().join("codey/provider-sync.json");

        let plan = provider_sync_plan_at(temp.path(), "codey_global", &marker).unwrap();

        assert_eq!(plan, ProviderSyncPlan::Full);
        assert!(!marker.exists());
    }

    #[test]
    fn legacy_sync_is_adopted_after_fast_provider_validation() {
        let temp = tempfile::tempdir().unwrap();
        write_rollout(temp.path(), "rollout-thread-1.jsonl", "codey_global");
        write_legacy_sync(temp.path(), "codey_global");
        let marker = temp.path().join("codey/provider-sync.json");

        let plan = provider_sync_plan_at(temp.path(), "codey_global", &marker).unwrap();

        assert_eq!(plan, ProviderSyncPlan::Cached);
        assert_eq!(
            read_marker(&marker).unwrap().target_provider,
            "codey_global"
        );
    }

    #[test]
    fn provider_change_invalidates_cached_sync() {
        let temp = tempfile::tempdir().unwrap();
        write_rollout(temp.path(), "rollout-thread-1.jsonl", "openai");
        let marker = temp.path().join("codey/provider-sync.json");
        write_marker(&marker, "codey_global").unwrap();

        let plan = provider_sync_plan_at(temp.path(), "codey_global", &marker).unwrap();

        assert_eq!(plan, ProviderSyncPlan::Full);
    }
}
