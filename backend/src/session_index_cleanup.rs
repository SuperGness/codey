use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SESSION_DIRS: [&str; 2] = ["sessions", "archived_sessions"];
const BACKUP_KEEP_COUNT: usize = 5;
const MANAGED_BY: &str = "Codey session index cleanup";

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexCleanupReport {
    pub scanned_entries: usize,
    pub live_threads: usize,
    pub pruned_entries: usize,
    pub backup_dir: Option<String>,
}

#[derive(Debug, Clone)]
struct CleanupCandidate {
    id: String,
    thread_name: String,
    updated_at: String,
}

#[derive(Debug)]
struct CleanupPlan {
    path: PathBuf,
    original_bytes: Vec<u8>,
    original_text: String,
    snapshot_sha256: String,
    scanned_entries: usize,
    candidates: Vec<CleanupCandidate>,
}

struct CleanupLock {
    path: PathBuf,
}

impl CleanupLock {
    fn acquire(home: &Path) -> Result<Self> {
        let path = home.join("tmp/codey-session-index-cleanup.lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(&path)
            .with_context(|| format!("会话索引清理锁已存在：{}", path.display()))?;
        fs::write(
            path.join("owner.json"),
            serde_json::to_vec(&json!({
                "pid": std::process::id(),
                "startedAt": timestamp_millis(),
            }))?,
        )?;
        Ok(Self { path })
    }
}

impl Drop for CleanupLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Removes exact-shape entries from `session_index.jsonl` when their thread ID
/// is absent from both rollout files and every known Codex SQLite reference.
///
/// This is intended to run before Codey launches its Codex instance. The
/// source snapshot is checked again immediately before an atomic replacement,
/// and the original index is backed up first.
pub fn cleanup(home: &Path) -> Result<SessionIndexCleanupReport> {
    if !home.exists() {
        return Ok(SessionIndexCleanupReport::default());
    }
    let _lock = CleanupLock::acquire(home)?;
    let live_thread_ids = collect_live_thread_ids(home)?;
    let Some(plan) = plan_cleanup(&home.join("session_index.jsonl"), &live_thread_ids)? else {
        return Ok(SessionIndexCleanupReport {
            live_threads: live_thread_ids.len(),
            ..SessionIndexCleanupReport::default()
        });
    };
    if plan.candidates.is_empty() {
        return Ok(SessionIndexCleanupReport {
            scanned_entries: plan.scanned_entries,
            live_threads: live_thread_ids.len(),
            ..SessionIndexCleanupReport::default()
        });
    }

    let selected_ids = plan
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<HashSet<_>>();
    let (next_text, pruned_entries) = filtered_index_text(&plan, &selected_ids);
    let backup_dir = create_backup(home, &plan, pruned_entries)?;

    let current_bytes = fs::read(&plan.path)
        .with_context(|| format!("重新读取会话索引失败：{}", plan.path.display()))?;
    if current_bytes != plan.original_bytes {
        anyhow::bail!(
            "session_index.jsonl 在扫描后发生变化；为避免覆盖 Codex 新内容，本次清理已中止，备份位于 {}",
            backup_dir.display()
        );
    }

    atomic_write(&plan.path, next_text.as_bytes())
        .with_context(|| format!("原子写入会话索引失败：{}", plan.path.display()))?;
    prune_backups(home)?;
    Ok(SessionIndexCleanupReport {
        scanned_entries: plan.scanned_entries,
        live_threads: live_thread_ids.len(),
        pruned_entries,
        backup_dir: Some(backup_dir.to_string_lossy().to_string()),
    })
}

fn collect_live_thread_ids(home: &Path) -> Result<HashSet<String>> {
    let mut ids = HashSet::new();
    for path in rollout_files(home)? {
        if let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(rollout_thread_id_from_filename)
        {
            ids.insert(id);
            // Standard Codex rollout names already contain the authoritative
            // thread UUID. Avoid rereading the complete JSONL history merely
            // to discover the same session_meta id.
            continue;
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("读取 rollout 失败：{}", path.display()))?;
        for segment in text.split_inclusive('\n') {
            let (line, _) = split_line_ending(segment);
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if record.get("type").and_then(Value::as_str) != Some("session_meta") {
                continue;
            }
            if let Some(id) = record
                .get("payload")
                .and_then(Value::as_object)
                .and_then(|payload| payload.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                ids.insert(id.to_string());
            }
        }
    }
    for path in sqlite_paths(home)? {
        ids.extend(sqlite_thread_ids(&path)?);
    }
    Ok(ids)
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

fn rollout_thread_id_from_filename(name: &str) -> Option<String> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    if stem.len() < 36 {
        return None;
    }
    let candidate = &stem[stem.len() - 36..];
    let valid = candidate
        .chars()
        .enumerate()
        .all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
    valid.then(|| candidate.to_string())
}

fn sqlite_paths(home: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let sqlite_dir = home.join("sqlite");
    if sqlite_dir.exists() {
        for entry in fs::read_dir(&sqlite_dir)
            .with_context(|| format!("扫描 SQLite 目录失败：{}", sqlite_dir.display()))?
        {
            let path = entry?.path();
            if path.is_file()
                && matches!(
                    path.extension().and_then(OsStr::to_str),
                    Some("db" | "sqlite" | "sqlite3")
                )
            {
                paths.push(path);
            }
        }
    }
    let legacy = home.join("state_5.sqlite");
    if legacy.is_file() {
        paths.push(legacy);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn sqlite_thread_ids(path: &Path) -> Result<HashSet<String>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("只读打开 Codex 数据库失败：{}", path.display()))?;
    db.busy_timeout(Duration::from_secs(5))?;
    let mut ids = HashSet::new();
    for (table, column) in [
        ("threads", "id"),
        ("local_thread_catalog", "thread_id"),
        ("automation_runs", "thread_id"),
        ("inbox_items", "thread_id"),
        ("sessions", "id"),
        ("messages", "session_id"),
        ("thread_dynamic_tools", "thread_id"),
        ("thread_goals", "thread_id"),
        ("thread_spawn_edges", "parent_thread_id"),
        ("thread_spawn_edges", "child_thread_id"),
        ("stage1_outputs", "thread_id"),
        ("agent_job_items", "assigned_thread_id"),
    ] {
        if !table_columns(&db, table)?.contains(column) {
            continue;
        }
        let mut statement = db.prepare(&format!(
            "SELECT DISTINCT {column} FROM {table} WHERE COALESCE({column}, '') <> ''"
        ))?;
        ids.extend(
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?,
        );
    }
    Ok(ids)
}

fn table_columns(db: &Connection, table: &str) -> Result<HashSet<String>> {
    let escaped = table.replace('"', "\"\"");
    let mut statement = db.prepare(&format!("PRAGMA table_info(\"{escaped}\")"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?)
}

fn plan_cleanup(path: &Path, live_thread_ids: &HashSet<String>) -> Result<Option<CleanupPlan>> {
    if !path.exists() {
        return Ok(None);
    }
    let original_bytes =
        fs::read(path).with_context(|| format!("读取会话索引失败：{}", path.display()))?;
    let original_text = String::from_utf8(original_bytes.clone())
        .with_context(|| format!("会话索引不是 UTF-8：{}", path.display()))?;
    let mut candidates = Vec::new();
    let mut scanned_entries = 0;
    for segment in original_text.split_inclusive('\n') {
        let (line, _) = split_line_ending(segment);
        if let Some(candidate) = known_candidate(line) {
            scanned_entries += 1;
            if !live_thread_ids.contains(&candidate.id) {
                candidates.push(candidate);
            }
        }
    }
    Ok(Some(CleanupPlan {
        path: path.to_path_buf(),
        snapshot_sha256: sha256_hex(&original_bytes),
        original_bytes,
        original_text,
        scanned_entries,
        candidates,
    }))
}

fn known_candidate(line: &str) -> Option<CleanupCandidate> {
    let record = serde_json::from_str::<Value>(line).ok()?;
    let object = record.as_object()?;
    if object.len() != 3
        || !["id", "thread_name", "updated_at"]
            .iter()
            .all(|key| object.contains_key(*key))
    {
        return None;
    }
    let id = object.get("id")?.as_str()?.trim();
    let thread_name = object.get("thread_name")?.as_str()?;
    let updated_at = object.get("updated_at")?.as_str()?;
    if id.is_empty() || updated_at.trim().is_empty() {
        return None;
    }
    Some(CleanupCandidate {
        id: id.to_string(),
        thread_name: thread_name.to_string(),
        updated_at: updated_at.to_string(),
    })
}

fn filtered_index_text(plan: &CleanupPlan, selected_ids: &HashSet<String>) -> (String, usize) {
    let mut next = String::with_capacity(plan.original_text.len());
    let mut removed = 0;
    for segment in plan.original_text.split_inclusive('\n') {
        let (line, line_ending) = split_line_ending(segment);
        if known_candidate(line).is_some_and(|candidate| selected_ids.contains(&candidate.id)) {
            removed += 1;
        } else {
            next.push_str(line);
            next.push_str(line_ending);
        }
    }
    (next, removed)
}

fn create_backup(home: &Path, plan: &CleanupPlan, removed_entries: usize) -> Result<PathBuf> {
    let backup_root = home.join("backups_state/provider-sync");
    let backup_dir = unique_backup_dir(&backup_root);
    fs::create_dir_all(&backup_dir)?;
    fs::write(backup_dir.join("session_index.jsonl"), &plan.original_bytes)?;
    fs::write(
        backup_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "namespace": "codey-provider-sync-session-index-cleanup",
            "codexHome": home.to_string_lossy(),
            "createdAtMs": timestamp_millis(),
            "snapshotSha256": plan.snapshot_sha256,
            "prunedSessionIndexEntries": removed_entries,
            "candidates": plan.candidates.iter().map(|candidate| json!({
                "id": candidate.id,
                "threadName": candidate.thread_name,
                "updatedAt": candidate.updated_at,
            })).collect::<Vec<_>>(),
            "managedBy": MANAGED_BY,
        }))?,
    )?;
    Ok(backup_dir)
}

fn unique_backup_dir(root: &Path) -> PathBuf {
    let base = timestamp_millis().to_string();
    let mut path = root.join(&base);
    let mut suffix = 0usize;
    while path.exists() {
        suffix += 1;
        path = root.join(format!("{base}-{suffix}"));
    }
    path
}

fn prune_backups(home: &Path) -> Result<()> {
    let root = home.join("backups_state/provider-sync");
    if !root.exists() {
        return Ok(());
    }
    let mut managed = Vec::new();
    for entry in fs::read_dir(&root)? {
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
        if metadata.get("managedBy").and_then(Value::as_str) == Some(MANAGED_BY) {
            managed.push(path);
        }
    }
    managed.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    for path in managed.into_iter().skip(BACKUP_KEEP_COUNT) {
        let _ = fs::remove_dir_all(path);
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("路径没有父目录：{}", path.display()))?;
    let temp = parent.join(format!(
        ".{}.codey-{}-{}.tmp",
        path.file_name()
            .unwrap_or_else(|| OsStr::new("session-index"))
            .to_string_lossy(),
        std::process::id(),
        timestamp_millis()
    ));
    fs::write(&temp, bytes)?;
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&temp, metadata.permissions())?;
    }
    let replace_result = fs::rename(&temp, path);
    if let Err(error) = replace_result {
        #[cfg(windows)]
        {
            if path.exists() {
                fs::remove_file(path)?;
                fs::rename(&temp, path)?;
                return Ok(());
            }
        }
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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

    fn index_line(id: &str, name: &str) -> String {
        serde_json::to_string(&json!({
            "id": id,
            "thread_name": name,
            "updated_at": "2026-07-20T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn cleanup_prunes_only_exact_orphans_and_creates_a_backup() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let sessions = home.join("sessions/2026/07");
        let sqlite = home.join("sqlite");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&sqlite).unwrap();
        fs::write(
            sessions.join("rollout-live.jsonl"),
            serde_json::to_string(&json!({
                "type": "session_meta",
                "payload": {"id": "rollout-live", "model_provider": "openai"}
            }))
            .unwrap(),
        )
        .unwrap();
        let db = Connection::open(sqlite.join("codex.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE local_thread_catalog (thread_id TEXT);\
             INSERT INTO local_thread_catalog VALUES ('database-live');",
        )
        .unwrap();
        drop(db);

        let original = [
            index_line("orphan", "ghost"),
            index_line("rollout-live", "rollout"),
            index_line("database-live", "database"),
            serde_json::to_string(&json!({
                "id": "unknown-shape",
                "thread_name": "keep",
                "updated_at": "2026-07-20T00:00:00Z",
                "source": "cloud",
            }))
            .unwrap(),
            "not-json".to_string(),
        ]
        .join("\n")
            + "\n";
        fs::write(home.join("session_index.jsonl"), &original).unwrap();

        let report = cleanup(home).unwrap();
        assert_eq!(report.scanned_entries, 3);
        assert_eq!(report.pruned_entries, 1);
        assert!(report.live_threads >= 2);
        let updated = fs::read_to_string(home.join("session_index.jsonl")).unwrap();
        assert!(!updated.contains("\"id\":\"orphan\""));
        assert!(updated.contains("\"id\":\"rollout-live\""));
        assert!(updated.contains("\"id\":\"database-live\""));
        assert!(updated.contains("\"id\":\"unknown-shape\""));
        assert!(updated.contains("not-json"));

        let backup = PathBuf::from(report.backup_dir.unwrap());
        assert_eq!(
            fs::read_to_string(backup.join("session_index.jsonl")).unwrap(),
            original
        );
        let metadata: Value =
            serde_json::from_slice(&fs::read(backup.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata["managedBy"], MANAGED_BY);
        assert_eq!(metadata["prunedSessionIndexEntries"], 1);

        let second = cleanup(home).unwrap();
        assert_eq!(second.pruned_entries, 0);
        assert!(second.backup_dir.is_none());
    }

    #[test]
    fn rollout_filename_uuid_is_considered_live() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let sessions = home.join("archived_sessions");
        fs::create_dir_all(&sessions).unwrap();
        let id = "019eacb3-52e5-7b92-bf68-1108f0b4154c";
        fs::write(
            sessions.join(format!("rollout-2026-07-20T00-00-00-{id}.jsonl")),
            "{}\n",
        )
        .unwrap();
        fs::write(
            home.join("session_index.jsonl"),
            format!("{}\n", index_line(id, "filename-live")),
        )
        .unwrap();

        let report = cleanup(home).unwrap();
        assert_eq!(report.pruned_entries, 0);
        assert!(report.backup_dir.is_none());
    }
}
