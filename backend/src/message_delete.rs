use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

// Rollout JSONL lives under date-sharded folders; archived threads move to the
// sibling folder but keep the same rollout-<timestamp>-<thread-id>.jsonl name.
const ROLLOUT_SEARCH_DIRS: [&str; 2] = ["sessions", "archived_sessions"];
const TOMBSTONE_VERSION: u32 = 1;
const TOMBSTONE_DIR: &str = ".codey-message-delete-tombstones-v1";
const TOMBSTONE_LOCK_FILE: &str = ".codey-message-delete-tombstones-v1.lock";
const THREAD_HISTORY_DB: &str = "thread_history_1.sqlite";
// Codex keeps these databases open while a conversation is loaded. A short
// wait avoids turning the normal handoff between the renderer and the writer
// into an intermittent delete failure.
const MESSAGE_DELETE_SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MESSAGE_DELETE_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(40), Duration::from_millis(160)];
static TOMBSTONE_LOCK: Mutex<()> = Mutex::new(());
type PendingDeleteTombstones = BTreeMap<String, (BTreeSet<String>, Vec<PathBuf>)>;
type ResolvedPersistentMessageIds = (BTreeSet<String>, Vec<(String, String)>);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDeleteResult {
    pub deleted: usize,
    pub resolved_message_ids: Vec<String>,
    pub unsupported_databases: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct MessageDeleteReplaySummary {
    pub deleted: usize,
    pub cleared_sessions: usize,
    pub failures: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageDeleteTombstone {
    version: u32,
    session_id: String,
    message_id: String,
}

struct MessageDeleteLock {
    _file: fs::File,
    _thread_guard: MutexGuard<'static, ()>,
}

pub fn delete_messages_persistently(
    home: &Path,
    session_id: &str,
    message_ids: &[String],
) -> Result<MessageDeleteResult> {
    let session_id = crate::session_metadata::normalize_session_id(session_id)
        .trim()
        .to_string();
    let message_ids = message_ids
        .iter()
        .map(|message_id| normalize_message_id(message_id))
        .filter(|message_id| !message_id.is_empty())
        .collect::<BTreeSet<_>>();
    if session_id.is_empty() || message_ids.is_empty() {
        anyhow::bail!("session_id 和 message_ids 不能为空");
    }

    // Persist the intent before touching the rollout. A loaded Codex thread
    // can flush stale history after this request returns; startup maintenance
    // replays the tombstone after that writer has been stopped.
    let _guard = lock_message_deletes(home)?;
    let (message_ids, resolved_tail_aliases) =
        resolve_persistent_message_ids(home, &session_id, &message_ids)?;
    record_delete_tombstones_unlocked(home, &session_id, &message_ids)?;
    record_resolved_tail_aliases_unlocked(home, &session_id, &resolved_tail_aliases)?;
    let message_ids = message_ids.into_iter().collect::<Vec<_>>();
    let mut completed_ids = HashSet::new();
    let mut attempt = 0;
    loop {
        match delete_messages_once(home, &session_id, &message_ids) {
            Ok(mut outcome) => {
                completed_ids.extend(outcome.deleted_ids);
                outcome.result.deleted = completed_ids.len();
                return Ok(outcome.result);
            }
            Err(error)
                if attempt < MESSAGE_DELETE_RETRY_DELAYS.len()
                    && retryable_message_delete_error(&error.source) =>
            {
                completed_ids.extend(error.deleted_ids);
                std::thread::sleep(MESSAGE_DELETE_RETRY_DELAYS[attempt]);
                attempt += 1;
            }
            Err(error) => return Err(error.source),
        }
    }
}

struct DeleteMessagesAttemptOutcome {
    result: MessageDeleteResult,
    deleted_ids: HashSet<String>,
}

struct DeleteMessagesAttemptError {
    source: anyhow::Error,
    deleted_ids: HashSet<String>,
}

fn delete_messages_once(
    home: &Path,
    session_id: &str,
    message_ids: &[String],
) -> std::result::Result<DeleteMessagesAttemptOutcome, DeleteMessagesAttemptError> {
    if session_id.trim().is_empty() || message_ids.is_empty() {
        return Err(DeleteMessagesAttemptError {
            source: anyhow::anyhow!("session_id 和 message_ids 不能为空"),
            deleted_ids: HashSet::new(),
        });
    }
    let session_id = crate::session_metadata::normalize_session_id(session_id);
    let message_ids = message_ids
        .iter()
        .map(|message_id| normalize_message_id(message_id))
        .filter(|message_id| !message_id.is_empty())
        .collect::<Vec<_>>();
    if message_ids.is_empty() {
        return Err(DeleteMessagesAttemptError {
            source: anyhow::anyhow!("message_ids 不能为空"),
            deleted_ids: HashSet::new(),
        });
    }
    let mut result = MessageDeleteResult {
        deleted: 0,
        resolved_message_ids: message_ids.clone(),
        unsupported_databases: Vec::new(),
    };
    let mut deleted_ids = HashSet::new();
    let mut history_session_id = session_id.to_string();
    let has_rollout = match find_rollout_path(home, session_id) {
        Ok(Some(rollout_path)) => {
            if let Some(thread_id) = rollout_path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(rollout_thread_id_from_filename)
            {
                history_session_id = thread_id;
            }
            let selected = message_ids.iter().cloned().collect::<HashSet<_>>();
            match delete_turns_from_rollout(home, &rollout_path, &selected) {
                Ok(found) => {
                    deleted_ids.extend(found);
                    true
                }
                Err(source) => {
                    return Err(DeleteMessagesAttemptError {
                        source,
                        deleted_ids,
                    });
                }
            }
        }
        Ok(None) => false,
        Err(source) => {
            return Err(DeleteMessagesAttemptError {
                source,
                deleted_ids,
            });
        }
    };

    match delete_from_thread_history(
        &home.join(THREAD_HISTORY_DB),
        &history_session_id,
        &message_ids,
        has_rollout,
    ) {
        Ok(Some(found)) => deleted_ids.extend(found),
        Ok(None) if home.join(THREAD_HISTORY_DB).exists() => result
            .unsupported_databases
            .push(home.join(THREAD_HISTORY_DB).to_string_lossy().to_string()),
        Ok(None) => {}
        Err(source) => {
            return Err(DeleteMessagesAttemptError {
                source,
                deleted_ids,
            });
        }
    }
    result.deleted = deleted_ids.len();
    if has_rollout || result.deleted > 0 {
        return Ok(DeleteMessagesAttemptOutcome {
            result,
            deleted_ids,
        });
    }

    // Compatibility path for older Codex builds that stored individual
    // messages in SQLite instead of turn blocks in a rollout JSONL file.
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let Some(targets) =
            find_message_targets(&db_path).map_err(|source| DeleteMessagesAttemptError {
                source,
                deleted_ids: deleted_ids.clone(),
            })?
        else {
            result
                .unsupported_databases
                .push(db_path.to_string_lossy().to_string());
            continue;
        };
        let deleted =
            delete_from_db(&db_path, &targets, session_id, &message_ids).map_err(|source| {
                DeleteMessagesAttemptError {
                    source,
                    deleted_ids: deleted_ids.clone(),
                }
            })?;
        deleted_ids.extend(deleted);
    }
    result.deleted = deleted_ids.len();
    Ok(DeleteMessagesAttemptOutcome {
        result,
        deleted_ids,
    })
}

fn retryable_message_delete_error(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("会话记录已变化")
        || message.contains("database is locked")
        || message.contains("database table is locked")
        || message.contains("sqlite_busy")
}

pub(crate) fn reapply_persisted_deletions(home: &Path) -> Result<MessageDeleteReplaySummary> {
    let _guard = lock_message_deletes(home)?;
    let pending = read_delete_tombstones_unlocked(home)?;
    let mut summary = MessageDeleteReplaySummary::default();

    for (session_id, (message_ids, paths)) in pending {
        let message_ids = message_ids.into_iter().collect::<Vec<_>>();
        match delete_messages(home, &session_id, &message_ids) {
            Ok(result) if result.unsupported_databases.is_empty() => {
                summary.deleted += result.deleted;
                match remove_delete_tombstones(&paths) {
                    Ok(()) => summary.cleared_sessions += 1,
                    Err(error) => summary.failures.push((session_id, format!("{error:#}"))),
                }
            }
            Ok(result) => {
                // 未识别结构的库可能只是目录型 catalog。记录文件已不存在、被删
                // 轮次也查不到时，删除意图已经无从落地，清掉失效墓碑，不再每次
                // 启动重复报 patch_failed。仍可能存在时保持保守失败。
                if deleted_turns_absent_from_every_store(home, &session_id, &message_ids) {
                    match remove_delete_tombstones(&paths) {
                        Ok(()) => {
                            summary.cleared_sessions += 1;
                            eprintln!("会话已不存在，清理了失效的消息删除墓碑：{session_id}");
                        }
                        Err(error) => summary.failures.push((session_id, format!("{error:#}"))),
                    }
                } else {
                    summary.failures.push((
                        session_id,
                        format!(
                            "无法确认消息删除是否已落地；{} 个数据库结构不受支持（{}），且会话记录或数据库仍可能保留已删除的对话轮",
                            result.unsupported_databases.len(),
                            result
                                .unsupported_databases
                                .iter()
                                .map(|path| {
                                    let path = Path::new(path);
                                    path.strip_prefix(home)
                                        .unwrap_or(path)
                                        .to_string_lossy()
                                        .to_string()
                                })
                                .collect::<Vec<_>>()
                                .join(", "),
                        ),
                    ));
                }
            }
            Err(error) => summary.failures.push((session_id, format!("{error:#}"))),
        }
    }

    Ok(summary)
}

fn lock_message_deletes(home: &Path) -> Result<MessageDeleteLock> {
    let thread_guard = TOMBSTONE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("消息删除墓碑锁已损坏"))?;
    fs::create_dir_all(home)
        .with_context(|| format!("创建 Codex 数据目录失败：{}", home.display()))?;
    let path = home.join(TOMBSTONE_LOCK_FILE);
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("打开消息删除墓碑锁失败：{}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("获取消息删除墓碑锁失败：{}", path.display()))?;
    Ok(MessageDeleteLock {
        _file: file,
        _thread_guard: thread_guard,
    })
}

fn record_delete_tombstones_unlocked(
    home: &Path,
    session_id: &str,
    message_ids: &BTreeSet<String>,
) -> Result<()> {
    let directory = tombstone_directory(home);
    create_private_directory(&directory)?;
    for message_id in message_ids {
        let tombstone = MessageDeleteTombstone {
            version: TOMBSTONE_VERSION,
            session_id: session_id.to_string(),
            message_id: message_id.to_string(),
        };
        let path = tombstone_path(&directory, session_id, message_id);
        if path.exists() {
            let existing: MessageDeleteTombstone = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("解析消息删除墓碑失败：{}", path.display()))?;
            if existing != tombstone {
                anyhow::bail!("消息删除墓碑哈希冲突：{}", path.display());
            }
            continue;
        }
        let temp = crate::fs_util::unique_temp_path(&path);
        let bytes = serde_json::to_vec(&tombstone)?;
        if let Err(error) = write_private_file(&temp, &bytes) {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        match fs::rename(&temp, &path) {
            Ok(()) => {}
            Err(_error) if path.exists() => {
                let _ = fs::remove_file(&temp);
                let existing = fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                if existing != Some(tombstone) {
                    anyhow::bail!("消息删除墓碑哈希冲突：{}", path.display());
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(error)
                    .with_context(|| format!("保存消息删除墓碑失败：{}", path.display()));
            }
        }
    }
    sync_directory(&directory)
}

fn record_resolved_tail_aliases_unlocked(
    home: &Path,
    session_id: &str,
    aliases: &[(String, String)],
) -> Result<()> {
    if aliases.is_empty() {
        return Ok(());
    }
    let directory = tombstone_directory(home);
    create_private_directory(&directory)?;
    for (selector, message_id) in aliases {
        let tombstone = MessageDeleteTombstone {
            version: TOMBSTONE_VERSION,
            session_id: session_id.to_string(),
            message_id: message_id.to_string(),
        };
        let path = tombstone_path(&directory, session_id, selector);
        if path.exists() {
            let existing: MessageDeleteTombstone = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("解析消息删除尾部别名失败：{}", path.display()))?;
            if existing == tombstone {
                continue;
            }
            let is_legacy_selector = existing.version == TOMBSTONE_VERSION
                && existing.session_id == session_id
                && normalize_message_id(&existing.message_id) == normalize_message_id(selector);
            if !is_legacy_selector {
                anyhow::bail!("消息删除尾部别名哈希冲突：{}", path.display());
            }
        }
        let bytes = serde_json::to_vec(&tombstone)?;
        crate::fs_util::atomic_write_private(&path, &bytes)
            .with_context(|| format!("保存消息删除尾部别名失败：{}", path.display()))?;
    }
    sync_directory(&directory)
}

fn read_resolved_tail_alias_unlocked(
    home: &Path,
    session_id: &str,
    selector: &str,
) -> Result<Option<String>> {
    let path = tombstone_path(&tombstone_directory(home), session_id, selector);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let tombstone: MessageDeleteTombstone = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析消息删除尾部别名失败：{}", path.display()))?;
    if tombstone.version != TOMBSTONE_VERSION || tombstone.session_id != session_id {
        anyhow::bail!("无效的消息删除尾部别名：{}", path.display());
    }
    let message_id = normalize_message_id(&tombstone.message_id);
    if message_id.is_empty() {
        anyhow::bail!("无效的消息删除尾部别名：{}", path.display());
    }
    Ok((message_id != normalize_message_id(selector)).then_some(message_id))
}

fn read_delete_tombstones_unlocked(home: &Path) -> Result<PendingDeleteTombstones> {
    let directory = tombstone_directory(home);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取消息删除墓碑目录失败：{}", directory.display()));
        }
    };
    let mut pending = BTreeMap::<String, (BTreeSet<String>, Vec<PathBuf>)>::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let tombstone: MessageDeleteTombstone = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("解析消息删除墓碑失败：{}", path.display()))?;
        if tombstone.version != TOMBSTONE_VERSION
            || tombstone.session_id.trim().is_empty()
            || tombstone.message_id.trim().is_empty()
        {
            anyhow::bail!("无效的消息删除墓碑：{}", path.display());
        }
        let message_id = normalize_message_id(&tombstone.message_id);
        if message_id.is_empty() {
            anyhow::bail!("无效的消息删除墓碑：{}", path.display());
        }
        let (message_ids, paths) = pending.entry(tombstone.session_id).or_default();
        message_ids.insert(message_id);
        paths.push(path);
    }
    Ok(pending)
}

fn remove_delete_tombstones(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("清理消息删除墓碑失败：{}", path.display()));
            }
        }
    }
    if let Some(directory) = paths.first().and_then(|path| path.parent()) {
        sync_directory(directory)?;
    }
    Ok(())
}

fn tombstone_directory(home: &Path) -> PathBuf {
    home.join(TOMBSTONE_DIR)
}

fn tombstone_path(directory: &Path, session_id: &str, message_id: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{session_id}\0{message_id}").as_bytes());
    directory.join(format!("{digest:x}.json"))
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("创建消息删除墓碑目录失败：{}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("创建消息删除墓碑临时文件失败：{}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("写入消息删除墓碑失败：{}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("同步消息删除墓碑失败：{}", path.display()))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn delete_messages(
    home: &Path,
    session_id: &str,
    message_ids: &[String],
) -> Result<MessageDeleteResult> {
    delete_messages_once(home, session_id, message_ids)
        .map(|outcome| outcome.result)
        .map_err(|error| error.source)
}

fn normalize_message_id(value: &str) -> String {
    let value = value.trim();
    value
        .rsplit_once(":turn:")
        .map(|(_, turn_id)| turn_id.trim())
        .unwrap_or(value)
        .to_string()
}

fn tail_message_index(value: &str) -> Option<usize> {
    value
        .strip_prefix("history-content:tail:")?
        .split(':')
        .next()?
        .parse()
        .ok()
}

fn resolve_persistent_message_ids(
    home: &Path,
    session_id: &str,
    message_ids: &BTreeSet<String>,
) -> Result<ResolvedPersistentMessageIds> {
    let mut tail_message_ids = message_ids
        .iter()
        .filter_map(|message_id| tail_message_index(message_id).map(|index| (message_id, index)))
        .collect::<Vec<_>>();
    if tail_message_ids.is_empty() {
        return Ok((message_ids.clone(), Vec::new()));
    }
    tail_message_ids.sort_by_key(|(_, index)| *index);
    if tail_message_ids
        .iter()
        .enumerate()
        .any(|(expected, (_, index))| expected != *index)
    {
        anyhow::bail!("只能安全解析从当前页面末轮开始连续选择的尾部轮次");
    }

    let mut stable_turn_ids = vec![None; tail_message_ids.len()];
    for (tail_message_id, index) in &tail_message_ids {
        stable_turn_ids[*index] =
            read_resolved_tail_alias_unlocked(home, session_id, tail_message_id)?;
    }
    if stable_turn_ids.iter().any(Option::is_none) {
        let rollout_path =
            find_rollout_path(home, session_id)?.context("找不到页面尾部轮次对应的会话记录")?;
        let canonical_rollout = canonical_rollout_path(home, &rollout_path)?;
        let original = fs::read_to_string(&canonical_rollout)
            .with_context(|| format!("读取会话记录失败：{}", canonical_rollout.display()))?;
        let rollout_tail_ids = stable_rollout_tail_turn_ids(&original, tail_message_ids.len())
            .context("所选页面尾部轮次尚未稳定写入完整会话记录")?;
        for (index, stable_turn_id) in stable_turn_ids.iter_mut().enumerate() {
            match stable_turn_id {
                Some(existing) if existing != &rollout_tail_ids[index] => {
                    anyhow::bail!("页面尾部轮次别名与当前会话尾部不一致，已拒绝重新解析");
                }
                None => *stable_turn_id = Some(rollout_tail_ids[index].clone()),
                Some(_) => {}
            }
        }
    }

    let mut resolved = message_ids.clone();
    let mut aliases = Vec::with_capacity(tail_message_ids.len());
    for (tail_message_id, index) in tail_message_ids {
        let stable_turn_id = stable_turn_ids[index]
            .take()
            .context("页面尾部轮次缺少稳定会话标识")?;
        resolved.remove(tail_message_id);
        resolved.insert(stable_turn_id.clone());
        aliases.push((tail_message_id.clone(), stable_turn_id));
    }
    Ok((resolved, aliases))
}

fn find_rollout_path(home: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    for db_path in codex_session_db_paths_from_home(home) {
        if !db_path.exists() {
            continue;
        }
        let connection = open_message_delete_database(&db_path)?;
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
            let path = if path.is_absolute() {
                path
            } else {
                home.join(path)
            };
            // The catalog can briefly retain the old location while Codex
            // moves a thread between sessions/ and archived_sessions/. Fall
            // through to the filename scan instead of failing on a stale
            // catalog path.
            if path.is_file() {
                return Ok(Some(path));
            }
        }
    }
    // Some Codex builds only keep a catalog row (or none at all) and never map
    // the thread to its rollout file. The filename itself embeds the thread id,
    // so the rollout can still be located without any database cooperation.
    Ok(find_rollout_file_by_session_id(home, session_id))
}

pub(crate) fn find_rollout_file_by_session_id(home: &Path, session_id: &str) -> Option<PathBuf> {
    find_rollout_files_by_session_id(home, session_id)
        .into_iter()
        .next()
}

pub(crate) fn find_rollout_files_by_session_id(home: &Path, session_id: &str) -> Vec<PathBuf> {
    let mut matches = Vec::new();
    for dirname in ROLLOUT_SEARCH_DIRS {
        let root = home.join(dirname);
        if !root.exists() {
            continue;
        }
        let mut files = Vec::new();
        if let Err(error) = collect_rollout_files(&root, &mut files) {
            // A broken fallback must not fail the whole delete; the SQLite path
            // above is still authoritative whenever it can answer.
            eprintln!("扫描会话目录失败：{error:#}");
        }
        // 目录顺序即优先级：活跃会话目录命中后才看归档目录，同一目录内按
        // 路径排序保证确定性（日期分片下最早文件优先）。
        files.sort();
        matches.extend(files.into_iter().filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    rollout_thread_id_from_filename(name)
                        .is_some_and(|thread_id| thread_id.eq_ignore_ascii_case(session_id))
                })
        }));
    }
    matches
}

fn collect_rollout_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        fs::read_dir(root).with_context(|| format!("扫描会话目录失败：{}", root.display()))?
    {
        let entry = entry?;
        // 不跟随目录链接，避免跨目录扫描或循环。
        if entry.file_type()?.is_symlink() {
            continue;
        }
        let path = entry.path();
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
    let candidate = stem.get(stem.len() - 36..)?;
    let valid = candidate
        .chars()
        .enumerate()
        .all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
    valid.then(|| candidate.to_string())
}

/// 逻辑扫描的库大小上限。超大库宁可保留墓碑保守失败，也不在启动路径上全表扫描。
const MAX_DATABASE_SCAN_BYTES: u64 = 256 * 1024 * 1024;

/// 会话记录文件已找不到时，判断删除意图是否已无处可落：被删的轮次不在任何存储里，
/// 且线程历史以外的库也不再引用该会话（引用说明线程仍存活，记录文件可能只是暂时
/// 找不到）。线程历史只是记录文件的派生投影，Codex 整体删除线程后可能残留其他轮次，
/// 这些残留不会让已删轮次复活，因此那里只按轮次查。非 uuid 形态的 id 无法通过
/// 文件名匹配验证，一律保持保守（false）。
fn deleted_turns_absent_from_every_store(
    home: &Path,
    session_id: &str,
    turn_ids: &[String],
) -> bool {
    if rollout_thread_id_from_filename(&format!("rollout-anchor-{session_id}.jsonl"))
        .is_none_or(|thread_id| thread_id != session_id)
    {
        return false;
    }
    if find_rollout_file_by_session_id(home, session_id).is_some() {
        return false;
    }
    !databases_reference_deleted_turns(home, session_id, turn_ids)
}

/// 查证只看现存行：已删除的数据会留在 SQLite 空闲页里，按原始字节扫描会把这些
/// 残留误判为仍被引用，导致墓碑永远无法清除。任何无法查证的情况都按「仍被引用」处理。
fn databases_reference_deleted_turns(home: &Path, session_id: &str, turn_ids: &[String]) -> bool {
    let thread_history = home.join(THREAD_HISTORY_DB);
    match thread_history_references_turns(&thread_history, session_id, turn_ids) {
        Ok(Some(true)) | Err(_) => return true,
        Ok(Some(false)) => {}
        Ok(None) => {
            if database_rows_reference_any(&thread_history, session_id, turn_ids).unwrap_or(true) {
                return true;
            }
        }
    }
    codex_session_db_paths_from_home(home)
        .iter()
        .any(|path| database_rows_reference_any(path, session_id, turn_ids).unwrap_or(true))
}

/// `None` 表示库不存在或结构未识别，由调用方回落到逐列查证。
fn thread_history_references_turns(
    path: &Path,
    session_id: &str,
    turn_ids: &[String],
) -> Result<Option<bool>> {
    if !path.exists() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(MESSAGE_DELETE_SQLITE_BUSY_TIMEOUT)?;
    let mut turn_tables = Vec::new();
    for table in sqlite_table_names(&connection)? {
        let columns = crate::sqlite_util::table_columns(&connection, &table)?;
        if columns.contains("thread_id") && columns.contains("turn_id") {
            turn_tables.push(table);
        }
    }
    if !["thread_items", "thread_turns"]
        .iter()
        .all(|required| turn_tables.iter().any(|table| table == required))
    {
        return Ok(None);
    }
    if turn_ids.is_empty() {
        return Ok(Some(false));
    }
    let placeholders = std::iter::repeat_n("?", turn_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut values = vec![session_id.to_string()];
    values.extend(turn_ids.iter().cloned());
    for table in turn_tables {
        let escaped = table.replace('"', "\"\"");
        let referenced: bool = connection.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM \"{escaped}\" WHERE thread_id = ? AND turn_id IN ({placeholders}))"
            ),
            params_from_iter(values.iter()),
            |row| row.get(0),
        )?;
        if referenced {
            return Ok(Some(true));
        }
    }
    Ok(Some(false))
}

/// 结构未识别的库逐表逐列查找会话或被删轮次的标识。带 `threads(id)` 表的状态库
/// 只以该表判断会话是否存活：子代理线程等元数据会在父会话删除后继续提到它的 id。
/// SQLite 打开时会合并 wal 中已提交的内容。
fn database_rows_reference_any(path: &Path, session_id: &str, turn_ids: &[String]) -> Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_DATABASE_SCAN_BYTES {
        return Ok(true);
    }
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(MESSAGE_DELETE_SQLITE_BUSY_TIMEOUT)?;
    let tables = sqlite_table_names(&connection)?;
    let mut needles = turn_ids.to_vec();
    if tables.iter().any(|table| table == "threads")
        && crate::sqlite_util::table_columns(&connection, "threads")?.contains("id")
    {
        let alive: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?1)",
            [session_id],
            |row| row.get(0),
        )?;
        if alive {
            return Ok(true);
        }
    } else {
        needles.push(session_id.to_string());
    }
    for table in tables {
        let columns = crate::sqlite_util::table_columns(&connection, &table)?;
        if columns.is_empty() {
            continue;
        }
        let condition = columns
            .iter()
            .map(|column| {
                let column = column.replace('"', "\"\"");
                format!("instr(CAST(\"{column}\" AS TEXT), ?1) > 0")
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let table = table.replace('"', "\"\"");
        let mut statement = connection.prepare(&format!(
            "SELECT EXISTS(SELECT 1 FROM \"{table}\" WHERE {condition})"
        ))?;
        for needle in needles.iter().filter(|needle| !needle.is_empty()) {
            if statement.query_row([needle], |row| row.get::<_, bool>(0))? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn sqlite_table_names(connection: &Connection) -> Result<Vec<String>> {
    let mut statement =
        connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names)
}

fn delete_turns_from_rollout(
    home: &Path,
    rollout_path: &Path,
    selected: &HashSet<String>,
) -> Result<HashSet<String>> {
    let canonical_rollout = canonical_rollout_path(home, rollout_path)?;
    recover_rollout_write(&canonical_rollout)?;
    let metadata = fs::metadata(&canonical_rollout)?;
    let original = fs::read_to_string(&canonical_rollout)
        .with_context(|| format!("读取会话记录失败：{}", canonical_rollout.display()))?;
    let mut output = String::with_capacity(original.len());
    let mut removing_turn = false;
    let mut selected_turn_seen = false;
    let mut found = HashSet::new();
    for line in original.split_inclusive('\n') {
        let json_line = line.trim_end_matches(['\r', '\n']);
        // 先做子串预筛：边界行必然携带 "turn_context"/"task_started" 字面量，
        // 不满足的行绝不可能是边界，避免对每行做完整 serde_json::Value 解析。
        if (json_line.contains("\"turn_context\"") || json_line.contains("\"task_started\""))
            && let Some(turn_id) = turn_boundary_id(json_line)
        {
            removing_turn = selected.contains(&turn_id);
            if removing_turn {
                selected_turn_seen = true;
                found.insert(turn_id);
            }
        }
        // A later compaction snapshot may contain the deleted turn inside its
        // encrypted summary.  It cannot be edited safely, so discard the
        // snapshot and let Codex rebuild history from the remaining rollout.
        if selected_turn_seen
            && json_line.contains("\"compacted\"")
            && is_compacted_summary(json_line)
        {
            continue;
        }
        if !removing_turn {
            output.push_str(line);
        }
    }
    if found.is_empty() {
        return Ok(found);
    }

    rewrite_if_unchanged(
        &canonical_rollout,
        &metadata,
        original.as_bytes(),
        output.as_bytes(),
    )
    .with_context(|| format!("写回会话记录失败：{}", canonical_rollout.display()))?;
    Ok(found)
}

fn canonical_rollout_path(home: &Path, rollout_path: &Path) -> Result<PathBuf> {
    let canonical_home = home
        .canonicalize()
        .with_context(|| format!("找不到 Codex 数据目录：{}", home.display()))?;
    let canonical_rollout = rollout_path
        .canonicalize()
        .with_context(|| format!("找不到会话记录：{}", rollout_path.display()))?;
    if !canonical_rollout.starts_with(&canonical_home) {
        anyhow::bail!("会话记录不在 Codex 数据目录内，已拒绝修改");
    }
    Ok(canonical_rollout)
}

fn stable_rollout_tail_turn_ids(rollout: &str, count: usize) -> Option<Vec<String>> {
    let mut turns = Vec::<(String, bool)>::new();
    for line in rollout.lines() {
        if let Some(turn_id) = turn_boundary_id(line)
            && turns.last().is_none_or(|(current, _)| current != &turn_id)
        {
            turns.push((turn_id, false));
        }
        if let Some(turn_id) = terminal_turn_id(line)
            && let Some((current, terminal)) = turns.last_mut()
            && current == &turn_id
        {
            *terminal = true;
        }
    }
    if count > turns.len() {
        return None;
    }
    let tail = &turns[turns.len() - count..];
    tail.iter().all(|(_, terminal)| *terminal).then(|| {
        tail.iter()
            .rev()
            .map(|(turn_id, _)| turn_id.clone())
            .collect()
    })
}

fn terminal_turn_id(line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?;
    match payload.get("type").and_then(Value::as_str) {
        Some("task_complete" | "turn_aborted") => payload
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|turn_id| !turn_id.is_empty())
            .map(str::to_string),
        _ => None,
    }
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

fn turn_boundary_id(line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    let payload = value.get("payload")?;
    let record_type = value.get("type").and_then(Value::as_str);
    let is_boundary = record_type == Some("turn_context")
        || (record_type == Some("event_msg")
            && payload.get("type").and_then(Value::as_str) == Some("task_started"));
    is_boundary
        .then(|| payload.get("turn_id").and_then(Value::as_str))
        .flatten()
        .map(str::trim)
        .filter(|turn_id| !turn_id.is_empty())
        .map(str::to_string)
}

fn same_rollout_metadata(original: &fs::Metadata, current: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if original.dev() != current.dev() || original.ino() != current.ino() {
            return false;
        }
    }
    current.is_file()
        && original.len() == current.len()
        && original.modified().ok() == current.modified().ok()
        && original.created().ok() == current.created().ok()
}

fn rewrite_if_unchanged(
    destination: &Path,
    metadata: &fs::Metadata,
    original: &[u8],
    contents: &[u8],
) -> Result<()> {
    let temporary = crate::fs_util::unique_temp_path(destination);
    let recovery_path = rollout_recovery_path(destination);
    anyhow::ensure!(
        !recovery_path.exists(),
        "会话存在待恢复写入，请先恢复后再删除"
    );
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&temporary)?;
        serde_json::to_writer(
            &mut output,
            &RolloutRecovery {
                version: 1,
                restoring: false,
                identity: rollout_identity(metadata),
                original: std::str::from_utf8(original)?,
                replacement: std::str::from_utf8(contents)?,
            },
        )?;
        output.sync_all()?;
        drop(output);
        let mut current = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(destination)?;
        current.try_lock_exclusive()?;
        let mut buffer = [0; 64 * 1024];
        let mut offset = 0;
        loop {
            let count = current.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            anyhow::ensure!(
                original.get(offset..offset + count) == Some(&buffer[..count]),
                "会话记录已变化，请重新加载后再删除"
            );
            offset += count;
        }
        anyhow::ensure!(
            offset == original.len()
                && same_rollout_metadata(metadata, &current.metadata()?)
                && same_rollout_metadata(metadata, &fs::symlink_metadata(destination)?),
            "会话记录已变化，请重新加载后再删除"
        );
        crate::fs_util::persist_temp_file(&temporary, &recovery_path)?;
        if let Err(error) = overwrite_rollout(&mut current, contents) {
            drop(current);
            return match recover_rollout_write(destination) {
                Ok(()) => Err(error.into()),
                Err(recovery_error) => Err(anyhow::anyhow!(
                    "会话写入失败：{error}；恢复失败：{recovery_error:#}；原始数据保存在 {}",
                    recovery_path.display()
                )),
            };
        }
        fs::remove_file(&recovery_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Serialize, Deserialize)]
struct RolloutRecovery<Text> {
    version: u32,
    #[serde(default)]
    restoring: bool,
    identity: String,
    original: Text,
    replacement: Text,
}

fn rollout_identity(metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    }
    #[cfg(not(unix))]
    {
        format!("{:?}", metadata.created().ok())
    }
}

fn rollout_recovery_path(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    destination.with_file_name(format!(".{name}.codey-delete-recovery.json"))
}

fn overwrite_rollout(file: &mut fs::File, contents: &[u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn recover_rollout_write(destination: &Path) -> Result<()> {
    let recovery_path = rollout_recovery_path(destination);
    let recovery = match fs::File::open(&recovery_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut recovery: RolloutRecovery<String> =
        serde_json::from_reader(std::io::BufReader::new(recovery))?;
    anyhow::ensure!(recovery.version == 1, "会话恢复记录版本无效");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)?;
    file.try_lock_exclusive()?;
    anyhow::ensure!(
        rollout_identity(&file.metadata()?) == recovery.identity,
        "会话文件身份已变化，保留恢复记录以供检查"
    );
    let mut current = Vec::new();
    (&mut file)
        .take(recovery.original.len().max(recovery.replacement.len()) as u64 + 1)
        .read_to_end(&mut current)?;
    if recovery.restoring {
        if !current.starts_with(recovery.original.as_bytes()) {
            anyhow::ensure!(
                recovery.original.as_bytes().starts_with(&current),
                "会话恢复期间内容已变化，保留恢复记录以供检查"
            );
            overwrite_rollout(&mut file, recovery.original.as_bytes())?;
        }
    } else if current != recovery.original.as_bytes()
        && !current.starts_with(recovery.replacement.as_bytes())
    {
        anyhow::ensure!(
            recovery.replacement.as_bytes().starts_with(&current),
            "会话内容已变化，保留恢复记录以供检查"
        );
        recovery.restoring = true;
        crate::fs_util::atomic_write_private(&recovery_path, &serde_json::to_vec(&recovery)?)?;
        overwrite_rollout(&mut file, recovery.original.as_bytes())?;
    }
    fs::remove_file(recovery_path)?;
    Ok(())
}

fn table_columns(path: &Path, table: &str) -> Result<std::collections::HashSet<String>> {
    let connection = open_message_delete_database(path)?;
    Ok(crate::sqlite_util::table_columns(&connection, table)?)
}

fn open_message_delete_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("打开会话数据库失败：{}", path.display()))?;
    connection
        .busy_timeout(MESSAGE_DELETE_SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("设置会话数据库等待时间失败：{}", path.display()))?;
    Ok(connection)
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
        let Some(id_column) = ["id", "message_id", "item_id"]
            .into_iter()
            .find(|candidate| columns.contains(*candidate))
        else {
            continue;
        };
        let Some(session_column) = ["session_id", "thread_id"]
            .into_iter()
            .find(|candidate| columns.contains(*candidate))
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
) -> Result<HashSet<String>> {
    let mut connection = open_message_delete_database(path)?;
    let placeholders = std::iter::repeat_n("?", message_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut values = message_ids.to_vec();
    values.push(session_id.to_string());
    let transaction = connection.transaction()?;
    let mut deleted = HashSet::new();
    for target in targets {
        let sql = format!(
            "DELETE FROM {} WHERE {} IN ({placeholders}) AND {} = ? RETURNING {}",
            target.table, target.id_column, target.session_column, target.id_column
        );
        let mut statement = transaction.prepare(&sql)?;
        let found = statement
            .query_map(params_from_iter(values.iter()), |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        deleted.extend(found);
    }
    // Only committed IDs count: a later store may fail after this one succeeds.
    transaction.commit()?;
    Ok(deleted)
}

fn delete_from_thread_history(
    path: &Path,
    session_id: &str,
    message_ids: &[String],
    has_rollout: bool,
) -> Result<Option<HashSet<String>>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut connection = open_message_delete_database(path)?;
    let mut tables = Vec::new();
    for table in ["thread_items", "thread_turns"] {
        let columns = crate::sqlite_util::table_columns(&connection, table)?;
        if columns.contains("thread_id") && columns.contains("turn_id") {
            tables.push(table);
        }
    }
    if tables.len() != 2 {
        return Ok(None);
    }

    let projection_columns =
        crate::sqlite_util::table_columns(&connection, "thread_history_projection_state")?;
    let invalidate_projection = has_rollout
        && [
            "thread_id",
            "next_rollout_byte_offset",
            "next_rollout_ordinal",
        ]
        .iter()
        .all(|column| projection_columns.contains(*column));
    let has_realtime_projection = invalidate_projection
        && crate::sqlite_util::table_columns(&connection, "thread_realtime_items")?
            .contains("thread_id");

    let placeholders = std::iter::repeat_n("?", message_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut values = message_ids.to_vec();
    values.push(session_id.to_string());
    let transaction = connection.transaction()?;
    let mut found = HashSet::new();
    for table in &tables {
        let filter = format!("turn_id IN ({placeholders}) AND thread_id = ?");
        let matched = {
            let mut statement = transaction.prepare(&format!(
                "SELECT DISTINCT turn_id FROM {table} WHERE {filter}"
            ))?;
            statement
                .query_map(params_from_iter(values.iter()), |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?
        };
        found.extend(matched);
        if invalidate_projection {
            // Removing rollout records invalidates byte positions for surviving
            // turns too. Rebuild this thread's derived history from its source,
            // including when startup replays an already-applied tombstone.
            transaction.execute(
                &format!("DELETE FROM {table} WHERE thread_id = ?"),
                [session_id],
            )?;
        } else {
            transaction.execute(
                &format!("DELETE FROM {table} WHERE {filter}"),
                params_from_iter(values.iter()),
            )?;
        }
    }
    if invalidate_projection {
        transaction.execute(
            "DELETE FROM thread_history_projection_state WHERE thread_id = ?",
            [session_id],
        )?;
        if has_realtime_projection {
            transaction.execute(
                "DELETE FROM thread_realtime_items WHERE thread_id = ?",
                [session_id],
            )?;
        }
    }
    transaction.commit()?;
    Ok(Some(found))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::sync::mpsc;
    use tempfile::tempdir;

    #[test]
    fn interrupted_rollout_write_recovers_original_without_losing_completed_appends() {
        let home = tempdir().unwrap();
        let path = home.path().join("rollout.jsonl");
        fs::write(&path, b"original-history").unwrap();
        let recovery = RolloutRecovery {
            version: 1,
            restoring: false,
            identity: rollout_identity(&fs::metadata(&path).unwrap()),
            original: "original-history",
            replacement: "kept-history",
        };
        let recovery_path = rollout_recovery_path(&path);
        for (interrupted, expected) in [
            ("", "original-history"),
            ("kept-", "original-history"),
            ("kept-history\nnew-turn", "kept-history\nnew-turn"),
        ] {
            fs::write(&recovery_path, serde_json::to_vec(&recovery).unwrap()).unwrap();
            fs::write(&path, interrupted).unwrap();
            recover_rollout_write(&path).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), expected);
            assert!(!recovery_path.exists());
        }
        fs::write(&recovery_path, serde_json::to_vec(&recovery).unwrap()).unwrap();
        fs::write(&path, "unrelated-change").unwrap();
        assert!(recover_rollout_write(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "unrelated-change");
        assert!(recovery_path.exists());
    }

    #[test]
    fn interrupted_recovery_handles_every_prefix_and_preserves_completed_appends() {
        let home = tempdir().unwrap();
        let path = home.path().join("rollout.jsonl");
        let original = "kept-history\nremoved-turn\n";
        fs::write(&path, original).unwrap();
        let recovery = RolloutRecovery {
            version: 1,
            restoring: true,
            identity: rollout_identity(&fs::metadata(&path).unwrap()),
            original,
            replacement: "kept-history\n",
        };
        let recovery_path = rollout_recovery_path(&path);
        for prefix_length in 0..=original.len() {
            fs::write(&recovery_path, serde_json::to_vec(&recovery).unwrap()).unwrap();
            fs::write(&path, &original[..prefix_length]).unwrap();
            recover_rollout_write(&path).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
            assert!(!recovery_path.exists());
        }
        fs::write(&recovery_path, serde_json::to_vec(&recovery).unwrap()).unwrap();
        let appended = format!("{original}new-turn\n");
        fs::write(&path, &appended).unwrap();
        recover_rollout_write(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), appended);
    }

    #[test]
    fn rewrite_rejects_changed_history_and_preserves_all_records() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        fs::write(&path, b"keep\nremove\n").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"new-record\n")
            .unwrap();
        assert!(rewrite_if_unchanged(&path, &metadata, b"keep\nremove\n", b"keep\n").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep\nremove\nnew-record\n");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn rewrite_commits_complete_history_and_rejects_same_length_changes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        fs::write(&path, b"original").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        assert!(rewrite_if_unchanged(&path, &metadata, b"different", b"new").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        rewrite_if_unchanged(&path, &metadata, b"original", b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn unverifiable_databases_preserve_uncertain_references() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(THREAD_HISTORY_DB);
        let session = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let turns = vec!["t1".to_string()];
        fs::write(&path, b"not a sqlite database").unwrap();
        assert!(!deleted_turns_absent_from_every_store(
            directory.path(),
            session,
            &turns
        ));
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_DATABASE_SCAN_BYTES + 1)
            .unwrap();
        assert!(database_rows_reference_any(&path, session, &turns).unwrap());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(!deleted_turns_absent_from_every_store(
            directory.path(),
            session,
            &turns
        ));
    }

    #[test]
    fn deleted_row_residue_in_free_pages_is_not_a_reference() {
        let directory = tempdir().unwrap();
        let session = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let path = directory.path().join("state_5.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA secure_delete = OFF;\
                 CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT);",
            )
            .unwrap();
        connection
            .execute("INSERT INTO threads VALUES (?1, 'deleted')", [session])
            .unwrap();
        connection
            .execute("DELETE FROM threads WHERE id = ?1", [session])
            .unwrap();
        drop(connection);
        let bytes = fs::read(&path).unwrap();
        assert!(
            bytes
                .windows(session.len())
                .any(|window| window == session.as_bytes()),
            "fixture must leave the deleted id in a free page"
        );

        assert!(deleted_turns_absent_from_every_store(
            directory.path(),
            session,
            &["t1".to_string()]
        ));
        // 父会话删除后，子代理线程的元数据仍会提到它的 id，这不代表父会话存活。
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO threads VALUES ('child', ?1)",
                [format!("{{\"parent_thread_id\":\"{session}\"}}")],
            )
            .unwrap();
        drop(connection);
        assert!(deleted_turns_absent_from_every_store(
            directory.path(),
            session,
            &["t1".to_string()]
        ));
        assert!(!deleted_turns_absent_from_every_store(
            directory.path(),
            session,
            &["child".to_string()]
        ));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("INSERT INTO threads VALUES (?1, 'live')", [session])
            .unwrap();
        drop(connection);
        assert!(!deleted_turns_absent_from_every_store(
            directory.path(),
            session,
            &["t1".to_string()]
        ));
    }

    #[test]
    fn oversized_thread_history_is_checked_by_thread_id_instead_of_bytes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(THREAD_HISTORY_DB);
        let present = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let absent = "019ff8aa-0b6e-7a01-a605-7a717a7795e4";
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE thread_turns (thread_id TEXT, turn_id TEXT);\
                 CREATE TABLE thread_items (thread_id TEXT, turn_id TEXT);",
            )
            .unwrap();
        connection
            .execute("INSERT INTO thread_items VALUES (?1, 't1')", [present])
            .unwrap();
        drop(connection);
        // 稀疏扩容越过字节扫描上限；SQLite 按文件头的页数读取，尾部零页不影响查询。
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_DATABASE_SCAN_BYTES + 1)
            .unwrap();
        let deleted = ["t1".to_string()];
        assert!(!deleted_turns_absent_from_every_store(
            directory.path(),
            present,
            &deleted
        ));
        assert!(deleted_turns_absent_from_every_store(
            directory.path(),
            absent,
            &deleted
        ));
        // 线程整体删除后残留的其他轮次不会让已删轮次复活。
        assert!(deleted_turns_absent_from_every_store(
            directory.path(),
            present,
            &["t2".to_string()]
        ));
    }

    #[test]
    fn rollout_filename_parser_handles_unicode_without_panicking() {
        for name in [
            format!("rollout-{}a.jsonl", "中".repeat(12)),
            "rollout-🚀.jsonl".into(),
            "rollout-short.jsonl".into(),
        ] {
            assert_eq!(rollout_thread_id_from_filename(&name), None);
        }
        let session = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        assert_eq!(
            rollout_thread_id_from_filename(&format!("rollout-2026-{session}.jsonl")),
            Some(session.into())
        );
    }

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
    fn legacy_deletion_counts_each_requested_id_once_across_stores() {
        let home = tempdir().unwrap();
        fs::create_dir(home.path().join("sqlite")).unwrap();
        let paths = [
            home.path().join("sqlite/codex.db"),
            home.path().join("state_5.sqlite"),
        ];
        for path in &paths {
            let connection = Connection::open(path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE threads (id TEXT PRIMARY KEY);\
                     CREATE TABLE messages (id TEXT, session_id TEXT);\
                     CREATE TABLE items (item_id TEXT, thread_id TEXT);\
                     INSERT INTO messages VALUES ('m1', 's1'), ('m2', 's1'), ('m1', 's2');\
                     INSERT INTO items VALUES ('m1', 's1'), ('m2', 's1'), ('m1', 's2');",
                )
                .unwrap();
        }

        let result =
            delete_messages_persistently(home.path(), "s1", &["m1".into(), "missing".into()])
                .unwrap();

        assert_eq!(result.deleted, 1);
        assert!(result.unsupported_databases.is_empty());
        for path in &paths {
            let connection = Connection::open(path).unwrap();
            for table in ["messages", "items"] {
                let remaining: usize = connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(remaining, 2);
            }
        }
    }

    #[test]
    fn legacy_deletion_does_not_count_rolled_back_records() {
        let home = tempdir().unwrap();
        let path = home.path().join("state_5.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE messages (id TEXT, session_id TEXT);\
                 CREATE TABLE items (item_id TEXT, thread_id TEXT);\
                 INSERT INTO messages VALUES ('m1', 's1');\
                 INSERT INTO items VALUES ('m1', 's1');\
                 CREATE TRIGGER reject_item_delete BEFORE DELETE ON items \
                 BEGIN SELECT RAISE(ABORT, 'test deletion failure'); END;",
            )
            .unwrap();
        drop(connection);

        let error = delete_messages_once(home.path(), "s1", &["m1".into()])
            .err()
            .unwrap();

        assert!(format!("{:#}", error.source).contains("test deletion failure"));
        assert!(error.deleted_ids.is_empty());
        let connection = Connection::open(path).unwrap();
        for table in ["messages", "items"] {
            let remaining: usize = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(remaining, 1);
        }
    }

    #[test]
    fn retries_after_legacy_lock_without_losing_committed_deletion_ids() {
        let home = tempdir().unwrap();
        fs::create_dir(home.path().join("sqlite")).unwrap();
        let first_path = home.path().join("sqlite/codex.db");
        let locked_path = home.path().join("state_5.sqlite");
        for (path, message_id) in [(&first_path, "m1"), (&locked_path, "m2")] {
            let connection = Connection::open(path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE threads (id TEXT PRIMARY KEY);\
                     CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT);",
                )
                .unwrap();
            connection
                .execute("INSERT INTO messages VALUES (?1, 's1')", [message_id])
                .unwrap();
        }
        assert_eq!(
            codex_session_db_paths_from_home(home.path()),
            vec![first_path.clone(), locked_path.clone()]
        );

        let (ready_tx, ready_rx) = mpsc::channel();
        let lock_path = locked_path.clone();
        let lock_thread = std::thread::spawn(move || {
            let lock = Connection::open(lock_path).unwrap();
            // Allow schema reads while preventing the second store's deletion.
            lock.execute_batch("BEGIN IMMEDIATE").unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(5_500));
            lock.execute_batch("COMMIT").unwrap();
        });
        ready_rx.recv().unwrap();

        let result = delete_messages_persistently(home.path(), "s1", &["m1".into(), "m2".into()]);
        lock_thread.join().unwrap();
        let result = result.unwrap();

        assert_eq!(result.deleted, 2);
        assert!(result.unsupported_databases.is_empty());
        for path in [&first_path, &locked_path] {
            let connection = Connection::open(path).unwrap();
            let remaining: usize = connection
                .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
                .unwrap();
            assert_eq!(remaining, 0);
        }
    }

    #[test]
    fn deletes_a_projected_turn_using_the_rollout_thread_id() {
        let home = tempdir().unwrap();
        let session_id = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let rollout_thread_id = "019ff8ab-0b6e-7a01-a605-7a717a7795e3";
        let rollout_dir = home.path().join("sessions/2026/09/03");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join(format!(
            "rollout-test-{session_id}_{rollout_thread_id}.jsonl"
        ));
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
            ),
        )
        .unwrap();
        let catalog = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        catalog
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL)",
                [],
            )
            .unwrap();
        catalog
            .execute(
                "INSERT INTO threads VALUES (?1, ?2)",
                params![session_id, rollout.to_string_lossy().to_string()],
            )
            .unwrap();
        drop(catalog);

        let history_path = home.path().join(THREAD_HISTORY_DB);
        let connection = Connection::open(&history_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE thread_turns (thread_id TEXT, turn_id TEXT, status TEXT);\
                 CREATE TABLE thread_items (thread_id TEXT, turn_id TEXT, item_id TEXT);\
                 INSERT INTO thread_turns VALUES ('019ff8ab-0b6e-7a01-a605-7a717a7795e3', 't1', 'completed'), ('019ff8ab-0b6e-7a01-a605-7a717a7795e3', 't2', 'completed');\
                 INSERT INTO thread_items VALUES ('019ff8ab-0b6e-7a01-a605-7a717a7795e3', 't1', 'm1'), ('019ff8ab-0b6e-7a01-a605-7a717a7795e3', 't2', 'm2');",
            )
            .unwrap();
        drop(connection);

        let result = delete_messages_persistently(home.path(), session_id, &["t1".into()]).unwrap();

        assert_eq!(result.deleted, 1);
        assert!(result.unsupported_databases.is_empty());
        let connection = Connection::open(history_path).unwrap();
        for table in ["thread_turns", "thread_items"] {
            assert_eq!(
                connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE turn_id = 't1'"),
                        [],
                        |row| row.get::<_, usize>(0),
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE turn_id = 't2'"),
                        [],
                        |row| row.get::<_, usize>(0),
                    )
                    .unwrap(),
                1
            );
        }
    }

    fn create_projection_history(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection.execute_batch(
            "CREATE TABLE thread_turns (thread_id TEXT, turn_id TEXT, rollout_byte_offset INTEGER, rollout_end_byte_offset INTEGER);\
             CREATE TABLE thread_items (thread_id TEXT, turn_id TEXT, item_id TEXT);\
             CREATE TABLE thread_history_projection_state (thread_id TEXT PRIMARY KEY, next_rollout_byte_offset INTEGER NOT NULL, next_rollout_ordinal INTEGER NOT NULL);\
             CREATE TABLE thread_realtime_items (thread_id TEXT, item_id TEXT);",
        ).unwrap();
        for thread_id in ["s1", "other"] {
            connection
                .execute(
                    "INSERT INTO thread_turns VALUES (?1, 't1', 100, 200), (?1, 't2', 200, 300)",
                    [thread_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO thread_items VALUES (?1, 't1', 'm1'), (?1, 't2', 'm2')",
                    [thread_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO thread_history_projection_state VALUES (?1, 300, 10)",
                    [thread_id],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO thread_realtime_items VALUES (?1, 'r1')",
                    [thread_id],
                )
                .unwrap();
        }
        connection
    }

    #[test]
    fn rollout_deletion_invalidates_only_its_thread_projection_and_replay_repairs_old_cursors() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/10/01");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-test.jsonl");
        fs::write(&rollout, concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
        )).unwrap();
        let catalog = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        catalog
            .execute_batch("CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL)")
            .unwrap();
        catalog
            .execute(
                "INSERT INTO threads VALUES ('s1', ?1)",
                [rollout.to_string_lossy().as_ref()],
            )
            .unwrap();
        let history = create_projection_history(&home.path().join(THREAD_HISTORY_DB));

        let result = delete_messages_persistently(home.path(), "s1", &["t1".into()]).unwrap();
        assert_eq!(result.deleted, 1);
        assert!(fs::read_to_string(&rollout).unwrap().contains("t2"));
        for (table, other_count) in [
            ("thread_turns", 2),
            ("thread_items", 2),
            ("thread_history_projection_state", 1),
            ("thread_realtime_items", 1),
        ] {
            let count = |thread_id: &str| {
                history
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE thread_id = ?1"),
                        [thread_id],
                        |row| row.get::<_, usize>(0),
                    )
                    .unwrap()
            };
            assert_eq!(count("s1"), 0, "{table}");
            assert_eq!(count("other"), other_count, "{table}");
        }

        // An old deletion can leave a cursor even after the selected turn has
        // disappeared from both the rollout and the projected turn rows.
        history
            .execute_batch(
                "INSERT INTO thread_turns VALUES ('s1', 't2', 200, 300);\
             INSERT INTO thread_items VALUES ('s1', 't2', 'm2');\
             INSERT INTO thread_history_projection_state VALUES ('s1', 300, 10);",
            )
            .unwrap();
        let replay = reapply_persisted_deletions(home.path()).unwrap();
        assert!(replay.failures.is_empty());
        assert_eq!(replay.deleted, 0);
        for table in [
            "thread_turns",
            "thread_items",
            "thread_history_projection_state",
        ] {
            assert_eq!(
                history
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE thread_id = 's1'"),
                        [],
                        |row| row.get::<_, usize>(0),
                    )
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn deleting_without_a_rollout_preserves_the_remaining_projected_history() {
        let home = tempdir().unwrap();
        let history = create_projection_history(&home.path().join(THREAD_HISTORY_DB));
        let result = delete_messages_persistently(home.path(), "s1", &["t1".into()]).unwrap();
        assert_eq!(result.deleted, 1);
        for table in [
            "thread_turns",
            "thread_items",
            "thread_history_projection_state",
            "thread_realtime_items",
        ] {
            assert_eq!(
                history
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE thread_id = 's1'"),
                        [],
                        |row| row.get::<_, usize>(0),
                    )
                    .unwrap(),
                1,
                "{table}"
            );
        }
    }

    #[test]
    fn falls_back_to_the_archived_rollout_when_catalog_path_is_stale() {
        let home = tempdir().unwrap();
        let session_id = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let rollout_dir = home.path().join("archived_sessions/2026/09/30");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join(format!("rollout-2026-09-30T09-08-00-{session_id}.jsonl"));
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
            ),
        )
        .unwrap();

        let catalog = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        catalog
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL)",
                [],
            )
            .unwrap();
        catalog
            .execute(
                "INSERT INTO threads VALUES (?1, ?2)",
                params![
                    session_id,
                    home.path().join("sessions/moved.jsonl").to_string_lossy()
                ],
            )
            .unwrap();
        drop(catalog);

        let result = delete_messages_persistently(home.path(), session_id, &["t1".into()]).unwrap();

        assert_eq!(result.deleted, 1);
        assert!(!fs::read_to_string(&rollout).unwrap().contains("t1"));
    }

    #[test]
    fn retries_after_history_lock_without_losing_rollout_deletion_count() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/09/30");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-retry-history-lock.jsonl");
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
            ),
        )
        .unwrap();
        let catalog = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        catalog
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL)",
                [],
            )
            .unwrap();
        catalog
            .execute(
                "INSERT INTO threads (id, rollout_path) VALUES (?1, ?2)",
                params!["s1", rollout.to_string_lossy().to_string()],
            )
            .unwrap();
        drop(catalog);

        let history_path = home.path().join(THREAD_HISTORY_DB);
        let history = Connection::open(&history_path).unwrap();
        history
            .execute_batch(
                "CREATE TABLE thread_turns (thread_id TEXT, turn_id TEXT, status TEXT);\
                 CREATE TABLE thread_items (thread_id TEXT, turn_id TEXT, item_id TEXT);",
            )
            .unwrap();
        drop(history);

        let (ready_tx, ready_rx) = mpsc::channel();
        let lock_path = history_path.clone();
        let lock_thread = std::thread::spawn(move || {
            let lock = Connection::open(lock_path).unwrap();
            lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(5_500));
            lock.execute_batch("COMMIT").unwrap();
        });
        ready_rx.recv().unwrap();

        let result = delete_messages_persistently(home.path(), "s1", &["t1".into()]).unwrap();

        lock_thread.join().unwrap();
        assert_eq!(result.deleted, 1);
        assert!(result.unsupported_databases.is_empty());
        assert!(!fs::read_to_string(&rollout).unwrap().contains("t1"));
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
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"compacted\",\"payload\":{\"replacement_history\":[{\"type\":\"message\",\"role\":\"user\",\"content\":[]},{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"old\"}]}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
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
        let result =
            delete_messages(home.path(), "local:s1", &["history-content:turn:t1".into()]).unwrap();
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
    fn resolves_a_current_tail_key_idempotently_before_deleting_the_last_turn() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/17");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-tail-key.jsonl");
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"keep\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"remove tail\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\",\"turn_id\":\"t2\"}}\n",
            ),
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

        let tail_key = "history-content:tail:0:local:temporary-id";
        record_delete_tombstones_unlocked(
            home.path(),
            "s1",
            &BTreeSet::from([tail_key.to_string()]),
        )
        .unwrap();
        let old_tombstone = tombstone_path(&tombstone_directory(home.path()), "s1", tail_key);

        let result = delete_messages_persistently(home.path(), "s1", &[tail_key.into()]).unwrap();

        assert_eq!(result.deleted, 1);
        assert_eq!(result.resolved_message_ids, ["t2"]);
        assert!(old_tombstone.exists());
        let alias: MessageDeleteTombstone =
            serde_json::from_slice(&fs::read(&old_tombstone).unwrap()).unwrap();
        assert_eq!(alias.message_id, "t2");
        assert!(tombstone_path(&tombstone_directory(home.path()), "s1", "t2").exists());
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(remaining.contains("t1"));
        assert!(remaining.contains("keep"));
        assert!(!remaining.contains("t2"));
        assert!(!remaining.contains("remove tail"));

        let repeated = delete_messages_persistently(home.path(), "s1", &[tail_key.into()]).unwrap();

        assert_eq!(repeated.deleted, 0);
        assert_eq!(repeated.resolved_message_ids, ["t2"]);
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(remaining.contains("t1"));
        assert!(remaining.contains("keep"));
    }

    #[test]
    fn resolves_consecutive_tail_keys_before_deleting_multiple_last_turns() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/18");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-multiple-tail-keys.jsonl");
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"keep\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"remove older tail\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\",\"turn_id\":\"t2\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t3\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t3\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"remove newest tail\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t3\"}}\n",
            ),
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

        let newest_tail_key = "history-content:tail:0:local:newest-temporary-id";
        let older_tail_key = "history-content:tail:1:local:older-temporary-id";
        let result = delete_messages_persistently(
            home.path(),
            "s1",
            &[older_tail_key.into(), newest_tail_key.into()],
        )
        .unwrap();

        assert_eq!(result.deleted, 2);
        assert_eq!(result.resolved_message_ids, ["t2", "t3"]);
        for (selector, expected) in [(newest_tail_key, "t3"), (older_tail_key, "t2")] {
            let alias: MessageDeleteTombstone = serde_json::from_slice(
                &fs::read(tombstone_path(
                    &tombstone_directory(home.path()),
                    "s1",
                    selector,
                ))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(alias.message_id, expected);
        }
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(remaining.contains("t1"));
        assert!(remaining.contains("keep"));
        assert!(!remaining.contains("t2"));
        assert!(!remaining.contains("remove older tail"));
        assert!(!remaining.contains("t3"));
        assert!(!remaining.contains("remove newest tail"));

        let repeated = delete_messages_persistently(
            home.path(),
            "s1",
            &[older_tail_key.into(), newest_tail_key.into()],
        )
        .unwrap();

        assert_eq!(repeated.deleted, 0);
        assert_eq!(repeated.resolved_message_ids, ["t2", "t3"]);
    }

    #[test]
    fn refuses_non_consecutive_tail_keys() {
        let home = tempdir().unwrap();
        let tail_key = "history-content:tail:1:local:temporary-id";

        let error =
            delete_messages_persistently(home.path(), "s1", &[tail_key.into()]).unwrap_err();

        assert!(error.to_string().contains("从当前页面末轮开始连续选择"));
        assert!(!tombstone_path(&tombstone_directory(home.path()), "s1", tail_key).exists());
    }

    #[test]
    fn refuses_to_mix_a_stale_tail_alias_with_an_unresolved_tail_key() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/18");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-stale-tail-alias.jsonl");
        let original = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
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

        let stale_tail_key = "history-content:tail:0:local:stale-temporary-id";
        let unresolved_tail_key = "history-content:tail:1:local:new-temporary-id";
        record_resolved_tail_aliases_unlocked(
            home.path(),
            "s1",
            &[(stale_tail_key.into(), "deleted-t3".into())],
        )
        .unwrap();

        let error = delete_messages_persistently(
            home.path(),
            "s1",
            &[stale_tail_key.into(), unresolved_tail_key.into()],
        )
        .unwrap_err();

        assert!(error.to_string().contains("别名与当前会话尾部不一致"));
        assert_eq!(fs::read_to_string(&rollout).unwrap(), original);
        assert!(!tombstone_path(&tombstone_directory(home.path()), "s1", "t1").exists());
        assert!(!tombstone_path(&tombstone_directory(home.path()), "s1", "t2").exists());
    }

    #[test]
    fn refuses_to_guess_a_tail_key_when_the_last_rollout_turn_is_not_terminal() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/17");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-unstable-tail-key.jsonl");
        let original = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
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

        let tail_key = "history-content:tail:0:local:temporary-id";
        let error =
            delete_messages_persistently(home.path(), "s1", &[tail_key.into()]).unwrap_err();

        assert!(error.to_string().contains("尚未稳定写入"));
        assert_eq!(fs::read_to_string(&rollout).unwrap(), original);
        assert!(!tombstone_path(&tombstone_directory(home.path()), "s1", tail_key).exists());
    }

    #[test]
    fn deletes_a_turn_context_when_task_started_is_absent() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/12");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-turn-context.jsonl");
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"remove context-only turn\"}]}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\"}}\n",
            ),
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

        let result = delete_messages(home.path(), "s1", &["t1".into()]).unwrap();

        assert_eq!(result.deleted, 1);
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(!remaining.contains("t1"));
        assert!(!remaining.contains("remove context-only turn"));
        assert!(remaining.contains("t2"));
    }

    #[test]
    fn deletes_an_unfinished_turn_by_stable_id_without_consuming_its_completed_successor() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/31");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-interrupted-continuation.jsonl");
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"interrupted\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"interrupted\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"remove interrupted turn\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"continued\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"continued\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"keep completed continuation\"}]}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"continued\"}}\n",
            ),
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

        let result = delete_messages_persistently(
            home.path(),
            "s1",
            &["history-content:turn:interrupted".into()],
        )
        .unwrap();

        assert_eq!(result.deleted, 1);
        assert_eq!(result.resolved_message_ids, ["interrupted"]);
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(!remaining.contains("remove interrupted turn"));
        assert!(!remaining.contains("\"turn_id\":\"interrupted\""));
        assert!(remaining.contains("keep completed continuation"));
        assert!(remaining.contains("\"turn_id\":\"continued\""));
        assert!(remaining.contains("\"type\":\"task_complete\""));
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

    #[test]
    fn startup_replays_a_persisted_delete_after_stale_history_is_flushed() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/12");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-persisted-delete.jsonl");
        let deleted_turn = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"must stay deleted\"}]}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
        );
        let retained_turn = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\"}}\n",
        );
        fs::write(
            &rollout,
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"s1\"}}}}\n{deleted_turn}{retained_turn}"
            ),
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
            delete_messages_persistently(home.path(), " local:s1 ", &[" t1 ".into()])
                .unwrap()
                .deleted,
            1
        );
        let tombstone_path = tombstone_path(&tombstone_directory(home.path()), "s1", "t1");
        let tombstone: MessageDeleteTombstone =
            serde_json::from_slice(&fs::read(&tombstone_path).unwrap()).unwrap();
        assert_eq!(tombstone.session_id, "s1");
        assert_eq!(tombstone.message_id, "t1");

        fs::OpenOptions::new()
            .append(true)
            .open(&rollout)
            .unwrap()
            .write_all(deleted_turn.as_bytes())
            .unwrap();

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.deleted, 1);
        assert_eq!(replay.cleared_sessions, 1);
        assert!(replay.failures.is_empty());
        assert!(!tombstone_path.exists());
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(!remaining.contains("t1"));
        assert!(!remaining.contains("must stay deleted"));
        assert!(remaining.contains("t2"));
    }

    #[test]
    fn startup_normalizes_and_consumes_an_old_prefixed_tombstone() {
        let home = tempdir().unwrap();
        let rollout_dir = home.path().join("sessions/2026/08/12");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join("rollout-prefixed-tombstone.jsonl");
        fs::write(
            &rollout,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
            ),
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

        let directory = tombstone_directory(home.path());
        create_private_directory(&directory).unwrap();
        let prefixed_id = "history-content:turn:t1";
        let old_path = tombstone_path(&directory, "s1", prefixed_id);
        write_private_file(
            &old_path,
            &serde_json::to_vec(&MessageDeleteTombstone {
                version: TOMBSTONE_VERSION,
                session_id: "s1".into(),
                message_id: prefixed_id.into(),
            })
            .unwrap(),
        )
        .unwrap();

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.deleted, 1);
        assert_eq!(replay.cleared_sessions, 1);
        assert!(!old_path.exists());
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(!remaining.contains("t1"));
        assert!(remaining.contains("t2"));
    }

    #[test]
    fn startup_keeps_tombstones_when_storage_cannot_be_confirmed() {
        let home = tempdir().unwrap();
        let db = home.path().join("state_5.sqlite");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL)",
                [],
            )
            .unwrap();
        drop(connection);

        let result = delete_messages_persistently(home.path(), "s1", &["t1".into()]).unwrap();
        assert_eq!(result.deleted, 0);
        assert!(!result.unsupported_databases.is_empty());
        let tombstone_path = tombstone_path(&tombstone_directory(home.path()), "s1", "t1");
        assert!(tombstone_path.exists());

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.deleted, 0);
        assert_eq!(replay.cleared_sessions, 0);
        assert_eq!(replay.failures.len(), 1);
        assert!(tombstone_path.exists());
    }

    #[test]
    fn replays_a_tombstone_when_only_the_rollout_filename_knows_the_session() {
        let home = tempdir().unwrap();
        let session_id = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let rollout_dir = home.path().join("sessions/2026/08/27");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout = rollout_dir.join(format!("rollout-2026-08-27T09-08-00-{session_id}.jsonl"));
        let original = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"019ff8aa\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t1\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"t2\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t2\"}}\n",
        );
        fs::write(&rollout, original).unwrap();

        let directory = tombstone_directory(home.path());
        create_private_directory(&directory).unwrap();
        write_private_file(
            &tombstone_path(&directory, session_id, "history-content:turn:t1"),
            &serde_json::to_vec(&MessageDeleteTombstone {
                version: TOMBSTONE_VERSION,
                session_id: session_id.into(),
                message_id: "history-content:turn:t1".into(),
            })
            .unwrap(),
        )
        .unwrap();

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.deleted, 1);
        assert_eq!(replay.cleared_sessions, 1);
        assert!(replay.failures.is_empty());
        assert!(!tombstone_path(&directory, session_id, "history-content:turn:t1").exists());
        let remaining = fs::read_to_string(&rollout).unwrap();
        assert!(!remaining.contains("t1"));
        assert!(remaining.contains("t2"));
    }

    #[test]
    fn drops_a_tombstone_once_the_whole_session_vanishes_from_every_store() {
        let home = tempdir().unwrap();
        let session_id = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let catalog = Connection::open(sqlite_dir.join("codex.db")).unwrap();
        catalog
            .execute("CREATE TABLE automation_runs (id TEXT PRIMARY KEY)", [])
            .unwrap();
        drop(catalog);

        let directory = tombstone_directory(home.path());
        create_private_directory(&directory).unwrap();
        let tombstone = tombstone_path(&directory, session_id, "history-content:turn:t1");
        write_private_file(
            &tombstone,
            &serde_json::to_vec(&MessageDeleteTombstone {
                version: TOMBSTONE_VERSION,
                session_id: session_id.into(),
                message_id: "history-content:turn:t1".into(),
            })
            .unwrap(),
        )
        .unwrap();

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.deleted, 0);
        assert_eq!(replay.cleared_sessions, 1);
        assert!(replay.failures.is_empty());
        assert!(!tombstone.exists());
    }

    #[test]
    fn drops_a_tombstone_when_only_unrelated_projected_turns_remain() {
        let home = tempdir().unwrap();
        let session_id = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let history = Connection::open(home.path().join(THREAD_HISTORY_DB)).unwrap();
        history
            .execute_batch(
                "CREATE TABLE thread_turns (thread_id TEXT, turn_id TEXT);\
                 CREATE TABLE thread_items (thread_id TEXT, turn_id TEXT);",
            )
            .unwrap();
        history
            .execute("INSERT INTO thread_turns VALUES (?1, 't0')", [session_id])
            .unwrap();
        drop(history);
        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let catalog = Connection::open(sqlite_dir.join("codex.db")).unwrap();
        catalog
            .execute("CREATE TABLE automation_runs (id TEXT PRIMARY KEY)", [])
            .unwrap();
        drop(catalog);

        let directory = tombstone_directory(home.path());
        create_private_directory(&directory).unwrap();
        let tombstone = tombstone_path(&directory, session_id, "t1");
        write_private_file(
            &tombstone,
            &serde_json::to_vec(&MessageDeleteTombstone {
                version: TOMBSTONE_VERSION,
                session_id: session_id.into(),
                message_id: "t1".into(),
            })
            .unwrap(),
        )
        .unwrap();

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.cleared_sessions, 1);
        assert!(replay.failures.is_empty());
        assert!(!tombstone.exists());
    }

    #[test]
    fn keeps_a_tombstone_while_any_store_still_mentions_the_session() {
        let home = tempdir().unwrap();
        let session_id = "019ff8aa-0b6e-7a01-a605-7a717a7795e3";
        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let catalog = Connection::open(sqlite_dir.join("codex.db")).unwrap();
        catalog
            .execute(
                "CREATE TABLE local_thread_catalog (thread_id TEXT NOT NULL)",
                [],
            )
            .unwrap();
        catalog
            .execute("INSERT INTO local_thread_catalog VALUES (?1)", [session_id])
            .unwrap();
        drop(catalog);

        let directory = tombstone_directory(home.path());
        create_private_directory(&directory).unwrap();
        let tombstone = tombstone_path(&directory, session_id, "history-content:turn:t1");
        write_private_file(
            &tombstone,
            &serde_json::to_vec(&MessageDeleteTombstone {
                version: TOMBSTONE_VERSION,
                session_id: session_id.into(),
                message_id: "history-content:turn:t1".into(),
            })
            .unwrap(),
        )
        .unwrap();

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.cleared_sessions, 0);
        assert_eq!(replay.failures.len(), 1);
        assert!(replay.failures[0].1.contains("无法确认消息删除是否已落地"));
        assert!(tombstone.exists());
    }

    #[test]
    fn startup_keeps_tombstones_after_a_partial_legacy_database_delete() {
        let home = tempdir().unwrap();
        let sqlite_dir = home.path().join("sqlite");
        fs::create_dir_all(&sqlite_dir).unwrap();
        let unsupported = Connection::open(sqlite_dir.join("codex.db")).unwrap();
        unsupported
            .execute("CREATE TABLE threads (id TEXT PRIMARY KEY)", [])
            .unwrap();
        drop(unsupported);
        let legacy = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        legacy
            .execute(
                "CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT NOT NULL)",
                [],
            )
            .unwrap();
        legacy
            .execute(
                "INSERT INTO messages (id, session_id) VALUES (?1, ?2)",
                params!["t1", "s1"],
            )
            .unwrap();
        drop(legacy);

        let result = delete_messages_persistently(home.path(), "s1", &["t1".into()]).unwrap();
        assert_eq!(result.deleted, 1);
        assert!(!result.unsupported_databases.is_empty());
        let tombstone_path = tombstone_path(&tombstone_directory(home.path()), "s1", "t1");
        assert!(tombstone_path.exists());

        let replay = reapply_persisted_deletions(home.path()).unwrap();

        assert_eq!(replay.cleared_sessions, 0);
        assert_eq!(replay.failures.len(), 1);
        assert!(tombstone_path.exists());
    }
}
