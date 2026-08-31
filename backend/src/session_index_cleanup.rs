use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::fs_util::{atomic_write_preserving_permissions as atomic_write, timestamp_millis};
use crate::sqlite_util::table_columns;

const SESSION_DIRS: [&str; 2] = ["sessions", "archived_sessions"];
const BACKUP_KEEP_COUNT: usize = 5;
const MANAGED_BY: &str = "Codey session index cleanup";
const CLEANUP_MARKER_VERSION: u32 = 1;
const CLEANUP_MARKER_FILE: &str = "tmp/codey-session-index-cleanup-marker-v1.json";
const SQLITE_ID_QUERY_CHUNK_SIZE: usize = 900;

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
    source_line_index: usize,
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

#[derive(Debug, Default)]
struct LiveThreadScan {
    ids: HashSet<String>,
    authoritative_sources: usize,
}

#[derive(Debug, Default)]
struct SqliteThreadScan {
    ids: HashSet<String>,
    has_reference_schema: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexSignature {
    len: u64,
    modified_ns: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CleanupMarker {
    version: u32,
    index: IndexSignature,
    validated_at_ms: u128,
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
    let index_path = home.join("session_index.jsonl");
    // collect_live_thread_ids walks every rollout and runs full-table scans over
    // each Codex database, so it must not run before we know the index has
    // entries that could actually be pruned.
    if !index_path.exists() {
        return Ok(SessionIndexCleanupReport::default());
    }
    if cleanup_marker_matches(home, &index_path) {
        return Ok(SessionIndexCleanupReport::default());
    }
    let _lock = CleanupLock::acquire(home)?;
    // A concurrent cleanup can finish between the lock-free fast-path check
    // above and acquiring the directory lock.
    if cleanup_marker_matches(home, &index_path) {
        return Ok(SessionIndexCleanupReport::default());
    }
    let Some(plan) = plan_cleanup_matching(&index_path, |_| true)? else {
        return Ok(SessionIndexCleanupReport::default());
    };
    if plan.candidates.is_empty() {
        record_cleanup_marker_for_unchanged_plan(home, &plan);
        return Ok(SessionIndexCleanupReport {
            scanned_entries: plan.scanned_entries,
            ..SessionIndexCleanupReport::default()
        });
    }
    let candidate_ids = plan
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<HashSet<_>>();
    let live_thread_scan = collect_live_thread_ids(home, &candidate_ids)?;
    // A discoverable schema (including an old empty catalog) is not evidence
    // that every indexed thread was deleted. If no candidate can be confirmed
    // live, keep the whole index and retry discovery on a later startup. Exact
    // user-requested deletions use remove_thread and do not need this inference.
    if live_thread_scan.authoritative_sources == 0 || live_thread_scan.ids.is_empty() {
        return Ok(SessionIndexCleanupReport {
            scanned_entries: plan.scanned_entries,
            ..SessionIndexCleanupReport::default()
        });
    }
    let live_thread_ids = live_thread_scan.ids;
    let plan = CleanupPlan {
        candidates: plan
            .candidates
            .into_iter()
            .filter(|candidate| !live_thread_ids.contains(&candidate.id))
            .collect(),
        ..plan
    };
    if plan.candidates.is_empty() {
        record_cleanup_marker_for_unchanged_plan(home, &plan);
        return Ok(SessionIndexCleanupReport {
            scanned_entries: plan.scanned_entries,
            live_threads: live_thread_ids.len(),
            ..SessionIndexCleanupReport::default()
        });
    }
    let report = apply_cleanup_plan(home, plan, live_thread_ids.len(), true)?;
    record_cleanup_marker(home, &index_path);
    Ok(report)
}

/// Removes one known thread from the legacy index as part of an explicit
/// deletion, regardless of stale catalog references that are being deleted in
/// the same operation.
pub fn remove_thread(home: &Path, thread_id: &str) -> Result<SessionIndexCleanupReport> {
    let thread_id = crate::session_metadata::normalize_session_id(thread_id);
    if !home.exists() || thread_id.is_empty() {
        return Ok(SessionIndexCleanupReport::default());
    }
    let _lock = CleanupLock::acquire(home)?;
    let Some(plan) = plan_cleanup_matching(&home.join("session_index.jsonl"), |candidate| {
        candidate.id == thread_id
    })?
    else {
        return Ok(SessionIndexCleanupReport::default());
    };
    apply_cleanup_plan(home, plan, 0, false)
}

fn apply_cleanup_plan(
    home: &Path,
    plan: CleanupPlan,
    live_threads: usize,
    backup_original: bool,
) -> Result<SessionIndexCleanupReport> {
    if plan.candidates.is_empty() {
        return Ok(SessionIndexCleanupReport {
            scanned_entries: plan.scanned_entries,
            live_threads,
            ..SessionIndexCleanupReport::default()
        });
    }

    let selected_line_indexes = plan
        .candidates
        .iter()
        .map(|candidate| candidate.source_line_index)
        .collect::<HashSet<_>>();
    let (next_text, pruned_entries) = filtered_index_text(&plan, &selected_line_indexes);
    let backup_dir = backup_original
        .then(|| create_backup(home, &plan, pruned_entries))
        .transpose()?;

    let current_bytes = fs::read(&plan.path)
        .with_context(|| format!("重新读取会话索引失败：{}", plan.path.display()))?;
    if current_bytes != plan.original_bytes {
        if let Some(backup_dir) = &backup_dir {
            anyhow::bail!(
                "session_index.jsonl 在扫描后发生变化；为避免覆盖 Codex 新内容，本次清理已中止，备份位于 {}",
                backup_dir.display()
            );
        }
        anyhow::bail!(
            "session_index.jsonl 在扫描后发生变化；为避免覆盖 Codex 新内容，本次删除已中止"
        );
    }

    atomic_write(&plan.path, next_text.as_bytes())
        .with_context(|| format!("原子写入会话索引失败：{}", plan.path.display()))?;
    if backup_original {
        prune_backups(home)?;
    }
    Ok(SessionIndexCleanupReport {
        scanned_entries: plan.scanned_entries,
        live_threads,
        pruned_entries,
        backup_dir: backup_dir.map(|path| path.to_string_lossy().to_string()),
    })
}

fn collect_live_thread_ids(home: &Path, candidate_ids: &HashSet<String>) -> Result<LiveThreadScan> {
    let mut scan = LiveThreadScan::default();
    'rollouts: for path in rollout_files(home)? {
        scan.authoritative_sources += 1;
        if let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(rollout_thread_id_from_filename)
        {
            if candidate_ids.contains(&id) {
                scan.ids.insert(id);
                if scan.ids.len() == candidate_ids.len() {
                    break 'rollouts;
                }
            }
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
                .filter(|id| candidate_ids.contains(*id))
            {
                scan.ids.insert(id.to_string());
                if scan.ids.len() == candidate_ids.len() {
                    break 'rollouts;
                }
            }
        }
    }
    for path in sqlite_paths(home)? {
        if scan.ids.len() == candidate_ids.len() {
            break;
        }
        let remaining = candidate_ids
            .difference(&scan.ids)
            .cloned()
            .collect::<HashSet<_>>();
        let sqlite_scan = sqlite_thread_ids(&path, &remaining)?;
        if sqlite_scan.has_reference_schema {
            scan.authoritative_sources += 1;
        }
        scan.ids.extend(sqlite_scan.ids);
    }
    Ok(scan)
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

fn sqlite_thread_ids(path: &Path, candidate_ids: &HashSet<String>) -> Result<SqliteThreadScan> {
    if candidate_ids.is_empty() {
        return Ok(SqliteThreadScan::default());
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("只读打开 Codex 数据库失败：{}", path.display()))?;
    db.busy_timeout(Duration::from_secs(5))?;
    let mut scan = SqliteThreadScan::default();
    let mut candidates = candidate_ids.iter().collect::<Vec<_>>();
    candidates.sort();
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
        scan.has_reference_schema = true;
        for chunk in candidates.chunks(SQLITE_ID_QUERY_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let mut statement = db.prepare(&format!(
                "SELECT DISTINCT {column} FROM {table} WHERE {column} IN ({placeholders})"
            ))?;
            scan.ids.extend(
                statement
                    .query_map(params_from_iter(chunk.iter().copied()), |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<rusqlite::Result<HashSet<_>>>()?,
            );
        }
    }
    Ok(scan)
}

fn cleanup_marker_path(home: &Path) -> PathBuf {
    home.join(CLEANUP_MARKER_FILE)
}

fn index_signature(path: &Path) -> Result<IndexSignature> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("读取会话索引元数据失败：{}", path.display()))?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
    Ok(IndexSignature {
        len: metadata.len(),
        modified_ns,
    })
}

fn cleanup_marker_matches(home: &Path, index_path: &Path) -> bool {
    let Ok(index) = index_signature(index_path) else {
        return false;
    };
    if index.modified_ns.is_none() {
        return false;
    }
    let Ok(bytes) = fs::read(cleanup_marker_path(home)) else {
        return false;
    };
    serde_json::from_slice::<CleanupMarker>(&bytes)
        .is_ok_and(|marker| marker.version == CLEANUP_MARKER_VERSION && marker.index == index)
}

fn write_cleanup_marker(home: &Path, index_path: &Path) -> Result<()> {
    let path = cleanup_marker_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let marker = CleanupMarker {
        version: CLEANUP_MARKER_VERSION,
        index: index_signature(index_path)?,
        validated_at_ms: timestamp_millis(),
    };
    atomic_write(&path, &serde_json::to_vec_pretty(&marker)?)
}

fn record_cleanup_marker(home: &Path, index_path: &Path) {
    // The marker is only a performance hint. A write failure must not turn a
    // successful, already-committed cleanup into a startup failure.
    let _ = write_cleanup_marker(home, index_path);
}

fn record_cleanup_marker_for_unchanged_plan(home: &Path, plan: &CleanupPlan) {
    if fs::read(&plan.path).is_ok_and(|current| current == plan.original_bytes) {
        record_cleanup_marker(home, &plan.path);
    }
}

fn plan_cleanup_matching(
    path: &Path,
    mut should_remove: impl FnMut(&CleanupCandidate) -> bool,
) -> Result<Option<CleanupPlan>> {
    if !path.exists() {
        return Ok(None);
    }
    let original_bytes =
        fs::read(path).with_context(|| format!("读取会话索引失败：{}", path.display()))?;
    let original_text = String::from_utf8(original_bytes.clone())
        .with_context(|| format!("会话索引不是 UTF-8：{}", path.display()))?;
    let mut candidates = Vec::new();
    let mut scanned_entries = 0;
    for (source_line_index, segment) in original_text.split_inclusive('\n').enumerate() {
        let (line, _) = split_line_ending(segment);
        if let Some(candidate) = known_candidate(line, source_line_index) {
            scanned_entries += 1;
            if should_remove(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    Ok(Some(CleanupPlan {
        path: path.to_path_buf(),
        snapshot_sha256: crate::fs_util::sha256_hex(&original_bytes),
        original_bytes,
        original_text,
        scanned_entries,
        candidates,
    }))
}

fn known_candidate(line: &str, source_line_index: usize) -> Option<CleanupCandidate> {
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
        source_line_index,
    })
}

fn filtered_index_text(
    plan: &CleanupPlan,
    selected_line_indexes: &HashSet<usize>,
) -> (String, usize) {
    let mut next = String::with_capacity(plan.original_text.len());
    let mut removed = 0;
    for (source_line_index, segment) in plan.original_text.split_inclusive('\n').enumerate() {
        let (line, line_ending) = split_line_ending(segment);
        if selected_line_indexes.contains(&source_line_index) {
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

fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
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
    fn explicit_delete_removes_the_selected_index_entry_despite_catalog_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        fs::write(
            home.join("session_index.jsonl"),
            [
                index_line("deleted-thread", "deleted"),
                index_line("kept-thread", "kept"),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let report = remove_thread(home, "local:deleted-thread").unwrap();

        assert_eq!(report.pruned_entries, 1);
        let updated = fs::read_to_string(home.join("session_index.jsonl")).unwrap();
        assert!(!updated.contains("\"id\":\"deleted-thread\""));
        assert!(updated.contains("\"id\":\"kept-thread\""));
        assert!(report.backup_dir.is_none());
        assert!(!home.join("backups_state").exists());
    }

    #[test]
    fn filtering_uses_planned_line_identity_and_preserves_original_endings() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session_index.jsonl");
        let removable = index_line("same-id", "remove");
        let retained = index_line("same-id", "keep");
        let unknown_shape = serde_json::to_string(&json!({
            "id": "same-id",
            "thread_name": "unknown",
            "updated_at": "2026-07-20T00:00:00Z",
            "source": "cloud",
        }))
        .unwrap();
        let original = format!("{removable}\r\n{retained}\n{unknown_shape}\nnot-json");
        fs::write(&path, &original).unwrap();

        let plan = plan_cleanup_matching(&path, |candidate| candidate.thread_name == "remove")
            .unwrap()
            .unwrap();
        let selected_line_indexes = plan
            .candidates
            .iter()
            .map(|candidate| candidate.source_line_index)
            .collect::<HashSet<_>>();
        let (filtered, removed) = filtered_index_text(&plan, &selected_line_indexes);

        assert_eq!(removed, 1);
        assert_eq!(filtered, format!("{retained}\n{unknown_shape}\nnot-json"));
    }

    #[test]
    fn explicit_delete_removes_all_exact_duplicate_entries() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let sqlite = home.join("sqlite");
        fs::create_dir_all(&sqlite).unwrap();
        Connection::open(sqlite.join("codex.db"))
            .unwrap()
            .execute("CREATE TABLE local_thread_catalog (thread_id TEXT)", [])
            .unwrap();
        fs::write(
            home.join("session_index.jsonl"),
            format!(
                "{}\n{}\n",
                index_line("duplicate-orphan", "first"),
                index_line("duplicate-orphan", "second")
            ),
        )
        .unwrap();

        let report = remove_thread(home, "duplicate-orphan").unwrap();

        assert_eq!(report.pruned_entries, 2);
        assert!(
            fs::read_to_string(home.join("session_index.jsonl"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cleanup_defers_when_no_authoritative_sources_are_available() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("sqlite")).unwrap();
        let original = format!(
            "{}\n{}\n",
            index_line("history-one", "first"),
            index_line("history-two", "second")
        );
        let index_path = home.join("session_index.jsonl");
        fs::write(&index_path, &original).unwrap();

        let report = cleanup(home).unwrap();

        assert_eq!(report.scanned_entries, 2);
        assert_eq!(report.pruned_entries, 0);
        assert!(report.backup_dir.is_none());
        assert_eq!(fs::read_to_string(index_path).unwrap(), original);
        assert!(!cleanup_marker_path(home).exists());
    }

    #[test]
    fn cleanup_preserves_history_when_catalog_cannot_confirm_any_indexed_thread() {
        for catalog_has_unrelated_thread in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path();
            let sqlite = home.join("sqlite");
            fs::create_dir_all(&sqlite).unwrap();
            let db = Connection::open(sqlite.join("codex.db")).unwrap();
            db.execute("CREATE TABLE local_thread_catalog (thread_id TEXT)", [])
                .unwrap();
            if catalog_has_unrelated_thread {
                db.execute(
                    "INSERT INTO local_thread_catalog VALUES ('other-thread')",
                    [],
                )
                .unwrap();
            }
            drop(db);
            // A newer or externally located state database need not be among
            // the known paths. An old catalog must not invalidate this history.
            let newer = Connection::open(home.join("state_6.sqlite")).unwrap();
            newer
                .execute_batch(
                    "CREATE TABLE threads (id TEXT); INSERT INTO threads VALUES ('history-one');",
                )
                .unwrap();
            drop(newer);
            let original = format!(
                "{}\n{}\n",
                index_line("history-one", "first"),
                index_line("history-two", "second")
            );
            let index = home.join("session_index.jsonl");
            fs::write(&index, &original).unwrap();

            let report = cleanup(home).unwrap();

            assert_eq!(report.pruned_entries, 0);
            assert!(report.backup_dir.is_none());
            assert_eq!(fs::read_to_string(&index).unwrap(), original);
            assert!(!cleanup_marker_path(home).exists());
        }
    }

    #[test]
    fn explicit_delete_aborts_if_another_writer_appends_after_planning() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let path = home.join("session_index.jsonl");
        fs::write(&path, format!("{}\n", index_line("target", "remove"))).unwrap();
        let plan = plan_cleanup_matching(&path, |candidate| candidate.id == "target")
            .unwrap()
            .unwrap();
        let appended = format!(
            "{}{}\n",
            plan.original_text,
            index_line("new-thread", "keep")
        );
        fs::write(&path, &appended).unwrap();

        assert!(apply_cleanup_plan(home, plan, 0, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), appended);
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

    #[test]
    fn unchanged_index_defers_cleanup_until_its_signature_changes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let sqlite = home.join("sqlite");
        fs::create_dir_all(&sqlite).unwrap();
        let database = sqlite.join("codex.db");
        let db = Connection::open(&database).unwrap();
        db.execute_batch(
            "CREATE TABLE local_thread_catalog (thread_id TEXT);\
             INSERT INTO local_thread_catalog VALUES ('temporarily-live');",
        )
        .unwrap();
        drop(db);
        let index = format!("{}\n", index_line("temporarily-live", "kept"));
        let index_path = home.join("session_index.jsonl");
        fs::write(&index_path, &index).unwrap();

        assert_eq!(cleanup(home).unwrap().pruned_entries, 0);
        assert!(cleanup_marker_path(home).exists());
        let db = Connection::open(&database).unwrap();
        db.execute("DELETE FROM local_thread_catalog", []).unwrap();
        drop(db);

        // The accepted gate semantics intentionally defer external-reference
        // changes while the legacy index itself is unchanged.
        assert_eq!(cleanup(home).unwrap().pruned_entries, 0);
        assert_eq!(fs::read_to_string(&index_path).unwrap(), index);

        fs::write(&index_path, format!("{index}\n")).unwrap();
        // A changed index triggers discovery, but an empty catalog cannot
        // prove that the last historical thread should be removed.
        assert_eq!(cleanup(home).unwrap().pruned_entries, 0);
        assert_eq!(
            fs::read_to_string(index_path).unwrap(),
            format!("{index}\n")
        );
    }

    #[test]
    fn sqlite_candidate_probe_chunks_large_id_sets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state_5.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute("CREATE TABLE messages (session_id TEXT)", [])
            .unwrap();
        db.execute(
            "INSERT INTO messages (session_id) VALUES (?1)",
            ["candidate-1001"],
        )
        .unwrap();
        drop(db);
        let candidates = (0..=1_001)
            .map(|index| format!("candidate-{index}"))
            .collect::<HashSet<_>>();

        let scan = sqlite_thread_ids(&path, &candidates).unwrap();

        assert!(scan.has_reference_schema);
        assert_eq!(scan.ids, HashSet::from(["candidate-1001".to_string()]));
    }
}
