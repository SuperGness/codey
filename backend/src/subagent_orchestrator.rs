use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::subagent::api::{TokenUsage, TraceContext};
use crate::subagent::lifecycle::{ExecutionOutcome, ExecutionPhase as ReservationState};
use crate::subagent::protocol::{AgentState, InterruptAcknowledgement, TerminalOutcome};
use crate::subagent::rules::{self, RoleAccess, RuleActor, RuleContext, RuleEffect, ToolClass};
use crate::subagent::telemetry::{
    self, ExecutionStatus, SubagentTraceEvent, TraceEventKind, TraceRecorder,
};

mod contract;
mod identity;

use contract::*;
use identity::*;

pub(crate) const POST_TOOL_HOOK_MATCHER: &str = "*";

const LEDGER_SCHEMA_VERSION: u32 = 15;
const MIN_LEDGER_SCHEMA_VERSION: u32 = 1;
const LEDGER_FILE: &str = "orchestrator-ledger-v1.json";
const LEDGER_LOCK_FILE: &str = "orchestrator-ledger-v1.lock";
const READ_ONLY_CONCURRENCY_LIMIT: usize = 3;
const WRITE_OR_MIXED_CONCURRENCY_LIMIT: usize = 2;
const MAX_RESERVATIONS_PER_LEDGER: usize = 1_024;
const DUPLICATE_TASK_ID_ERROR_CODE: &str = "CODEY_SUBAGENT_DUPLICATE_TASK_ID";
const LEDGER_CAPACITY_ERROR_CODE: &str = "CODEY_SUBAGENT_LEDGER_CAPACITY";
const FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT_ERROR_CODE: &str =
    "CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT";
const UNBOUND_ATTEMPT_ERROR_CODE: &str = "CODEY_SUBAGENT_UNBOUND_ATTEMPT";
const AGENT_ID_COLLISION_ERROR_CODE: &str = "CODEY_SUBAGENT_AGENT_ID_COLLISION";
const STALE_RUNTIME_ERROR_CODE: &str = "CODEY_SUBAGENT_STALE_RUNTIME_EVENT";
const CONCURRENCY_LIMIT_ERROR_CODE: &str = "CODEY_SUBAGENT_CONCURRENCY_LIMIT";
const MAX_RETIRED_RUNTIME_IDS: usize = 1_024;
const LEDGER_LOCK_TIMEOUT_MILLIS: u64 = 250;
const LEDGER_LOCK_RETRY_MILLIS: u64 = 5;
const MAX_TRANSCRIPT_METADATA_LINE_BYTES: usize = 1024 * 1024;
const MAX_SPAWN_RESPONSE_JSON_BYTES: usize = 64 * 1024;
const MAX_LEDGER_BYTES: u64 = 4 * 1024 * 1024;
const MAX_QUARANTINE_FILES_PER_SESSION: usize = 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SessionLedger {
    schema_version: u32,
    runtime_id_hash: String,
    #[serde(default = "default_runtime_generation")]
    runtime_generation: u64,
    #[serde(default)]
    retired_runtime_id_hashes: BTreeSet<String>,
    session_id_hash: String,
    revision: u64,
    created_at_ms: u64,
    updated_at_ms: u64,
    #[serde(default)]
    issued_task_ids: BTreeSet<String>,
    #[serde(default = "default_fencing_token")]
    next_fencing_token: u64,
    reservations: BTreeMap<String, Reservation>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RootHookContext<'a> {
    workspace_root: Option<&'a str>,
    active_agents: usize,
    now_ms: u64,
}

impl<'a> RootHookContext<'a> {
    pub(crate) fn new(workspace_root: Option<&'a str>, active_agents: usize, now_ms: u64) -> Self {
        Self {
            workspace_root,
            active_agents,
            now_ms,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Reservation {
    task_id: String,
    #[serde(default = "default_runtime_generation")]
    runtime_generation: u64,
    #[serde(default)]
    origin_runtime_id_hash: String,
    role: String,
    write_capable: bool,
    workspace_root: Option<String>,
    state: ReservationState,
    #[serde(default)]
    outcome: ExecutionOutcome,
    agent_id_hash: Option<String>,
    created_at_ms: u64,
    updated_at_ms: u64,
    #[serde(default)]
    trace_id: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    started_at_ms: Option<u64>,
    #[serde(default)]
    completed_at_ms: Option<u64>,
    #[serde(default)]
    token_usage: Option<TokenUsage>,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    attempt_id: String,
    #[serde(default)]
    fencing_token: u64,
    #[serde(default)]
    policy_revision: u64,
    #[serde(default)]
    fenced_at_ms: Option<u64>,
    #[serde(default)]
    spawn_failed: bool,
    #[serde(default)]
    pending_init_observed_at_ms: Option<u64>,
}

const fn default_fencing_token() -> u64 {
    1
}

const fn default_runtime_generation() -> u64 {
    1
}

struct LedgerStore {
    lock: File,
    ledger_path: PathBuf,
}

fn ledger_lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        // LockFileEx reports ERROR_LOCK_VIOLATION for an occupied byte range.
        || (cfg!(windows) && error.raw_os_error() == Some(33))
}

impl LedgerStore {
    fn open(state_root: &Path, session_id: &str) -> Result<Self> {
        fs::create_dir_all(state_root).with_context(|| {
            format!(
                "创建 Codey 子代理编排状态目录失败：{}",
                state_root.display()
            )
        })?;
        let session_hash = hash_component(session_id);
        let lock_path = state_root.join(LEDGER_LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("打开 Codey 子代理编排账本锁失败：{}", lock_path.display()))?;
        let lock_started = Instant::now();
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if ledger_lock_is_contended(&error) => {
                    if lock_started.elapsed() >= Duration::from_millis(LEDGER_LOCK_TIMEOUT_MILLIS) {
                        anyhow::bail!(
                            "获取 Codey 子代理编排账本锁超时（{} ms）：{}",
                            LEDGER_LOCK_TIMEOUT_MILLIS,
                            lock_path.display()
                        );
                    }
                    thread::sleep(Duration::from_millis(LEDGER_LOCK_RETRY_MILLIS));
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("获取 Codey 子代理编排账本锁失败：{}", lock_path.display())
                    });
                }
            }
        }
        let session_dir = state_root.join(session_hash);
        cleanup_stale_ledger_temps(&session_dir)?;
        Ok(Self {
            lock,
            ledger_path: session_dir.join(LEDGER_FILE),
        })
    }

    fn load(
        &self,
        runtime_id: &str,
        session_id: &str,
        now_ms: u64,
    ) -> Result<Option<SessionLedger>> {
        let Some(bytes) = self.read_bytes()? else {
            return Ok(None);
        };
        let mut ledger: SessionLedger = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "解析 Codey 子代理编排账本失败：{}",
                self.ledger_path.display()
            )
        })?;
        anyhow::ensure!(
            (MIN_LEDGER_SCHEMA_VERSION..=LEDGER_SCHEMA_VERSION).contains(&ledger.schema_version),
            "Codey 子代理编排账本版本不受支持：{}",
            ledger.schema_version
        );
        let source_schema_version = ledger.schema_version;
        let mut changed = migrate_ledger(&mut ledger, source_schema_version)?;
        let session_id_hash = hash_component(session_id);
        anyhow::ensure!(
            ledger.session_id_hash == session_id_hash,
            "Codey 子代理编排账本会话标识不一致"
        );
        let runtime_id_hash = hash_component(runtime_id);
        if ledger.runtime_id_hash != runtime_id_hash
            && ledger.retired_runtime_id_hashes.contains(&runtime_id_hash)
        {
            anyhow::bail!(
                "{STALE_RUNTIME_ERROR_CODE}: 收到已退役 runtime 的迟到子代理事件；已拒绝反向接管当前账本"
            );
        }
        if ledger.runtime_id_hash != runtime_id_hash {
            anyhow::ensure!(
                ledger.retired_runtime_id_hashes.len() < MAX_RETIRED_RUNTIME_IDS,
                "{STALE_RUNTIME_ERROR_CODE}: Codey 子代理账本记录的退役 runtime 已达到安全上限 {MAX_RETIRED_RUNTIME_IDS}"
            );
            anyhow::ensure!(
                ledger.runtime_generation < u64::MAX,
                "Codey 子代理 runtime generation 已耗尽"
            );
            let retired_runtime_id_hash = ledger.runtime_id_hash.clone();
            ledger.reservations.retain(|_, reservation| {
                // Failed spawn attempts never became provider-owned work and can
                // keep the legacy cross-runtime cleanup behavior. Every admitted
                // attempt remains as a settled tombstone so task/agent identity,
                // fencing and batch history cannot be replayed after a restart.
                !reservation.spawn_failed
            });
            for reservation in ledger.reservations.values_mut() {
                if reservation.state.is_active() {
                    reservation.state = ReservationState::Recovered;
                    if reservation.outcome == ExecutionOutcome::Unknown {
                        reservation.outcome = ExecutionOutcome::Lost;
                    }
                    reservation.updated_at_ms = now_ms;
                    reservation.completed_at_ms.get_or_insert(now_ms);
                    reservation.fenced_at_ms.get_or_insert(now_ms);
                    reservation.error_message.get_or_insert_with(|| {
                        "runtime generation changed before an authoritative successful outcome"
                            .to_string()
                    });
                }
                reservation.pending_init_observed_at_ms = None;
            }
            ledger
                .retired_runtime_id_hashes
                .insert(retired_runtime_id_hash);
            ledger.runtime_id_hash = runtime_id_hash;
            ledger.runtime_generation += 1;
            ledger.issued_task_ids = ledger.reservations.keys().cloned().collect();
            ledger.updated_at_ms = now_ms;
            changed = true;
        }
        validate_unique_agent_bindings(&ledger)?;
        if changed {
            self.save(&mut ledger, now_ms)?;
        }
        self.cleanup_retired_runtime_state(&ledger.retired_runtime_id_hashes)?;
        Ok(Some(ledger))
    }

    fn read_bytes(&self) -> Result<Option<Vec<u8>>> {
        match fs::symlink_metadata(&self.ledger_path) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= MAX_LEDGER_BYTES,
                "Codey 子代理编排账本不是可信的有界普通文件：{}",
                self.ledger_path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        crate::fs_util::read_bounded(&self.ledger_path, MAX_LEDGER_BYTES)
            .map(Some)
            .with_context(|| {
                format!(
                    "读取 Codey 子代理编排账本失败：{}",
                    self.ledger_path.display()
                )
            })
    }

    fn save(&self, ledger: &mut SessionLedger, now_ms: u64) -> Result<()> {
        // Never persist a state that the next Hook invocation would reject on
        // load. This also protects future write paths from bypassing
        // identity, capacity, or generation invariants.
        let _ = migrate_ledger(ledger, LEDGER_SCHEMA_VERSION)?;
        validate_unique_agent_bindings(ledger)?;
        let parent = self
            .ledger_path
            .parent()
            .context("Codey 子代理编排账本缺少父目录")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 Codey 子代理编排账本目录失败：{}", parent.display()))?;
        ledger.revision = ledger.revision.saturating_add(1);
        ledger.updated_at_ms = now_ms;
        let bytes = serde_json::to_vec(ledger).context("序列化 Codey 子代理编排账本失败")?;
        crate::fs_util::atomic_write_private(&self.ledger_path, &bytes).with_context(|| {
            format!(
                "原子替换 Codey 子代理编排账本失败：{}",
                self.ledger_path.display()
            )
        })
    }

    fn cleanup_retired_runtime_state(
        &self,
        retired_runtime_id_hashes: &BTreeSet<String>,
    ) -> Result<()> {
        if retired_runtime_id_hashes.is_empty() {
            return Ok(());
        }
        let session_dir = self
            .ledger_path
            .parent()
            .context("Codey 子代理编排账本缺少父目录")?;
        let entries = match fs::read_dir(session_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "读取 Codey 子代理退役 runtime 状态失败：{}",
                        session_dir.display()
                    )
                });
            }
        };
        let prefixes = retired_runtime_id_hashes
            .iter()
            .map(|runtime_id_hash| format!("{runtime_id_hash}-"))
            .collect::<Vec<_>>();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if !prefixes.iter().any(|prefix| file_name.starts_with(prefix)) {
                continue;
            }
            fs::remove_file(entry.path()).with_context(|| {
                format!(
                    "清理 Codey 子代理退役 runtime 状态失败：{}",
                    entry.path().display()
                )
            })?;
        }
        Ok(())
    }

    fn remove(&self) -> Result<()> {
        match fs::remove_file(&self.ledger_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "删除 Codey 子代理编排账本失败：{}",
                        self.ledger_path.display()
                    )
                });
            }
        }
        if let Some(parent) = self.ledger_path.parent() {
            match fs::remove_dir(parent) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("清理 Codey 子代理编排账本目录失败：{}", parent.display())
                    });
                }
            }
        }
        Ok(())
    }

    // SessionEnd 不带 runtime 归属信息；代次不一致且仍有活动预留时
    // 保留账本，交给所属代次或恢复逻辑处理，其余情况照常删除。
    fn remove_for_session_end(
        &self,
        runtime_id: &str,
        session_id: &str,
        now_ms: u64,
    ) -> Result<()> {
        let Some(bytes) = self.read_bytes()? else {
            return self.remove();
        };
        let mut ledger = match serde_json::from_slice::<SessionLedger>(&bytes) {
            Ok(ledger) if ledger.session_id_hash == hash_component(session_id) => ledger,
            Ok(_) => {
                self.quarantine_for_session_end(&bytes, "会话标识不一致")?;
                return Ok(());
            }
            Err(error) => {
                self.quarantine_for_session_end(&bytes, &format!("JSON 无法解析：{error}"))?;
                return Ok(());
            }
        };
        let runtime_id_hash = hash_component(runtime_id);
        if ledger.retired_runtime_id_hashes.contains(&runtime_id_hash) {
            // A retired process may deliver SessionEnd after a newer runtime has
            // already migrated and reopened the batch decision. It must not
            // delete the current owner's tombstones or decision state.
            return Ok(());
        }
        if ledger_has_outstanding(&ledger) && ledger.runtime_id_hash != runtime_id_hash {
            return Ok(());
        }
        if ledger_has_outstanding(&ledger) {
            let active_tasks = ledger
                .reservations
                .iter()
                .filter(|(_, reservation)| reservation.state.is_active())
                .map(|(task_id, _)| task_id.clone())
                .collect::<BTreeSet<_>>();
            if !active_tasks.is_empty() {
                fence_identity_conflict(
                    &mut ledger,
                    &active_tasks,
                    now_ms,
                    "SessionEnd arrived before an authoritative terminal outcome",
                );
            }
            self.save(&mut ledger, now_ms)?;
            return Ok(());
        }
        self.remove()
    }

    fn quarantine_for_session_end(&self, bytes: &[u8], reason: &str) -> Result<()> {
        let digest = hash_component_bytes(bytes);
        let quarantine_path = self.ledger_path.with_file_name(format!(
            "orchestrator-ledger-v1.corrupt-{}.json",
            &digest[..16]
        ));
        if quarantine_path.exists() {
            anyhow::ensure!(
                fs::read(&quarantine_path).ok().as_deref() == Some(bytes),
                "Codey 子代理损坏账本隔离目标冲突：{}",
                quarantine_path.display()
            );
            fs::remove_file(&self.ledger_path).with_context(|| {
                format!(
                    "移除已留存副本的 Codey 子代理损坏账本失败：{}",
                    self.ledger_path.display()
                )
            })?;
        } else {
            fs::rename(&self.ledger_path, &quarantine_path).with_context(|| {
                format!(
                    "隔离 Codey 子代理损坏账本失败：{} -> {}",
                    self.ledger_path.display(),
                    quarantine_path.display()
                )
            })?;
        }
        eprintln!(
            "Codey SessionEnd 已隔离不可读的子代理账本（{reason}）：{}",
            quarantine_path.display()
        );
        cleanup_old_quarantines(
            self.ledger_path
                .parent()
                .context("Codey 子代理编排账本缺少父目录")?,
        )?;
        Ok(())
    }
}

fn cleanup_stale_ledger_temps(session_dir: &Path) -> Result<()> {
    let entries = match fs::read_dir(session_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let prefix = format!(".{LEDGER_FILE}.codey-");
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".tmp"))
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn cleanup_old_quarantines(session_dir: &Path) -> Result<()> {
    let mut quarantines = fs::read_dir(session_dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with("orchestrator-ledger-v1.corrupt-") && name.ends_with(".json")
                })
        })
        .map(|entry| {
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, entry.path())
        })
        .collect::<Vec<_>>();
    quarantines.sort();
    let remove_count = quarantines
        .len()
        .saturating_sub(MAX_QUARANTINE_FILES_PER_SESSION);
    for (_, path) in quarantines.into_iter().take(remove_count) {
        fs::remove_file(path)?;
    }
    Ok(())
}

impl Drop for LedgerStore {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

impl SessionLedger {
    fn new(runtime_id: &str, session_id: &str, now_ms: u64) -> Self {
        Self {
            schema_version: LEDGER_SCHEMA_VERSION,
            runtime_id_hash: hash_component(runtime_id),
            runtime_generation: 1,
            retired_runtime_id_hashes: BTreeSet::new(),
            session_id_hash: hash_component(session_id),
            revision: 0,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            issued_task_ids: BTreeSet::new(),
            next_fencing_token: 1,
            reservations: BTreeMap::new(),
        }
    }
}

fn migrate_ledger(ledger: &mut SessionLedger, source_schema_version: u32) -> Result<bool> {
    let mut changed = false;
    let before = ledger.issued_task_ids.len();
    ledger
        .issued_task_ids
        .extend(ledger.reservations.keys().cloned());
    changed |= ledger.issued_task_ids.len() != before;
    if source_schema_version < 4 {
        let session_hash = ledger.session_id_hash.clone();
        for (task_id, reservation) in &mut ledger.reservations {
            if reservation.trace_id.is_empty() {
                reservation.trace_id = hash_component(&format!("{session_hash}:{task_id}"));
            }
            reservation.started_at_ms = match reservation.state {
                ReservationState::Running
                | ReservationState::Terminal
                | ReservationState::Recovered => Some(reservation.updated_at_ms),
                ReservationState::Pending | ReservationState::Failed => None,
            };
            reservation.completed_at_ms = matches!(
                reservation.state,
                ReservationState::Terminal | ReservationState::Failed | ReservationState::Recovered
            )
            .then_some(reservation.updated_at_ms);
        }
        changed = true;
    }
    if source_schema_version < 5 {
        let session_hash = ledger.session_id_hash.clone();
        let mut next_fencing_token = 1_u64;
        for (task_id, reservation) in &mut ledger.reservations {
            if reservation.attempt_id.is_empty() {
                reservation.attempt_id = hash_component(&format!(
                    "{session_hash}:{task_id}:{}",
                    reservation.created_at_ms
                ));
            }
            if reservation.fencing_token == 0 {
                reservation.fencing_token = next_fencing_token;
            }
            next_fencing_token = next_fencing_token
                .max(reservation.fencing_token)
                .saturating_add(1);
            match reservation.state {
                ReservationState::Failed => {
                    reservation.state = ReservationState::Terminal;
                    reservation.outcome = ExecutionOutcome::Failed;
                    reservation.spawn_failed = true;
                    reservation
                        .completed_at_ms
                        .get_or_insert(reservation.updated_at_ms);
                    reservation
                        .fenced_at_ms
                        .get_or_insert(reservation.updated_at_ms);
                }
                ReservationState::Recovered => {
                    if reservation.outcome == ExecutionOutcome::Unknown {
                        reservation.outcome = ExecutionOutcome::Lost;
                    }
                    reservation
                        .fenced_at_ms
                        .get_or_insert(reservation.updated_at_ms);
                }
                ReservationState::Terminal => {
                    // Schema v1-v4 did not persist an authoritative outcome. In
                    // particular, errored/shutdown/not_found were folded into
                    // the same phase as completed, so migration must not infer
                    // success from the old terminal bit.
                    reservation.outcome = ExecutionOutcome::Unknown;
                    reservation
                        .fenced_at_ms
                        .get_or_insert(reservation.updated_at_ms);
                }
                ReservationState::Pending | ReservationState::Running => {}
            }
        }
        ledger.next_fencing_token = ledger.next_fencing_token.max(next_fencing_token);
        changed = true;
    } else {
        let required_next = ledger
            .reservations
            .values()
            .map(|reservation| reservation.fencing_token)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        if ledger.next_fencing_token < required_next {
            ledger.next_fencing_token = required_next;
            changed = true;
        }
    }
    if source_schema_version < 9 {
        // Older ledgers did not track provider PendingInit observations per
        // reservation. Do not infer a timestamp from created/updated time: an
        // upgrade must never make an in-flight attempt immediately stale.
        for reservation in ledger.reservations.values_mut() {
            reservation.pending_init_observed_at_ms = None;
        }
        changed = true;
    }
    if source_schema_version < 10 {
        // v1-v9 had only the current runtime hash. Treat every existing
        // reservation as originating in generation 1; do not invent retired
        // owners or discard identity/batch history during the schema upgrade.
        ledger.runtime_generation = 1;
        ledger.retired_runtime_id_hashes.clear();
        let origin_runtime_id_hash = ledger.runtime_id_hash.clone();
        for reservation in ledger.reservations.values_mut() {
            reservation.runtime_generation = 1;
            reservation.origin_runtime_id_hash = origin_runtime_id_hash.clone();
        }
        changed = true;
    }
    anyhow::ensure!(
        ledger.runtime_generation > 0,
        "Codey 子代理账本缺少有效 runtime generation"
    );
    anyhow::ensure!(
        is_canonical_hash(&ledger.runtime_id_hash),
        "Codey 子代理账本当前 runtime 哈希格式无效"
    );
    anyhow::ensure!(
        ledger.retired_runtime_id_hashes.len() <= MAX_RETIRED_RUNTIME_IDS,
        "Codey 子代理账本退役 runtime 数量无效：{}",
        ledger.retired_runtime_id_hashes.len()
    );
    anyhow::ensure!(
        ledger
            .retired_runtime_id_hashes
            .iter()
            .all(|runtime_id_hash| is_canonical_hash(runtime_id_hash)),
        "Codey 子代理账本包含格式无效的退役 runtime 哈希"
    );
    anyhow::ensure!(
        !ledger
            .retired_runtime_id_hashes
            .contains(&ledger.runtime_id_hash),
        "Codey 子代理账本当前 runtime 同时被标记为已退役"
    );
    for reservation in ledger.reservations.values() {
        anyhow::ensure!(
            reservation.fencing_token > 0 && !reservation.attempt_id.is_empty(),
            "Codey 子代理编排账本缺少有效 attempt/fencing 元数据"
        );
        anyhow::ensure!(
            reservation.runtime_generation > 0
                && reservation.runtime_generation <= ledger.runtime_generation
                && is_canonical_hash(&reservation.origin_runtime_id_hash),
            "Codey 子代理编排账本缺少有效的 attempt runtime 归属"
        );
        anyhow::ensure!(
            (reservation.runtime_generation == ledger.runtime_generation
                && reservation.origin_runtime_id_hash == ledger.runtime_id_hash)
                || (reservation.runtime_generation < ledger.runtime_generation
                    && ledger
                        .retired_runtime_id_hashes
                        .contains(&reservation.origin_runtime_id_hash)),
            "Codey 子代理编排账本的 attempt runtime 代次与归属不一致"
        );
        anyhow::ensure!(
            !reservation.state.is_active()
                || (reservation.runtime_generation == ledger.runtime_generation
                    && reservation.origin_runtime_id_hash == ledger.runtime_id_hash),
            "Codey 子代理编排账本包含跨 runtime generation 的活动 attempt"
        );
        anyhow::ensure!(
            reservation.state.is_settled() || reservation.outcome == ExecutionOutcome::Unknown,
            "Codey 子代理编排账本的活动 phase 带有终态 outcome"
        );
        anyhow::ensure!(
            reservation.state.is_active() || reservation.pending_init_observed_at_ms.is_none(),
            "Codey 子代理编排账本的终态 reservation 带有 PendingInit 观察时间"
        );
    }
    if ledger.schema_version != LEDGER_SCHEMA_VERSION {
        ledger.schema_version = LEDGER_SCHEMA_VERSION;
        changed = true;
    }
    Ok(changed)
}

fn is_canonical_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn concurrency_denial(
    ledger: &SessionLedger,
    prepared: &PreparedContract,
    active_agents: usize,
) -> Option<String> {
    let tracked_active = ledger
        .reservations
        .values()
        .filter(|reservation| reservation.state.is_active() && !reservation.spawn_failed)
        .collect::<Vec<_>>();
    let tracked_active_count = tracked_active.len();
    let has_untracked_active = active_agents > tracked_active_count;
    let has_active_write = has_untracked_active
        || tracked_active
            .iter()
            .any(|reservation| reservation.write_capable);
    let candidate_is_read_only = prepared.policy.access == RoleAccess::ReadOnly;
    let limit = if candidate_is_read_only && !has_active_write {
        READ_ONLY_CONCURRENCY_LIMIT
    } else {
        WRITE_OR_MIXED_CONCURRENCY_LIMIT
    };
    let observed_active = active_agents.max(tracked_active_count);
    if observed_active < limit {
        return None;
    }
    let mode = if limit == READ_ONLY_CONCURRENCY_LIMIT {
        "已确认的纯只读批次"
    } else {
        "包含写入型或身份未确认代理的批次"
    };
    Some(format!(
        "{CONCURRENCY_LIMIT_ERROR_CODE}: Codey 子代理并发门禁：{mode}当前已有 {observed_active} 个活动代理，达到并发上限 {limit}。请先等待任一活动代理进入终态后再派发；该限制只约束同时运行数量，不限制后续批次或累计派发次数。"
    ))
}

fn recorded_delegation_count(ledger: &SessionLedger) -> usize {
    ledger.reservations.len().max(ledger.issued_task_ids.len())
}

fn ledger_capacity_denial(ledger: &SessionLedger) -> Option<String> {
    let recorded_tasks = recorded_delegation_count(ledger);
    if recorded_tasks < MAX_RESERVATIONS_PER_LEDGER {
        return None;
    }
    Some(format!(
        "{LEDGER_CAPACITY_ERROR_CODE}: Codey 本轮子代理账本已记录 {recorded_tasks} 个任务，达到安全上限 {MAX_RESERVATIONS_PER_LEDGER}。为保持任务 ID 防重放，Codey 不会静默删除历史记录；请停止继续派生，完成当前工作并通过 Stop 结算本轮。旧账本仍可读取，但在结算前不会继续增长。"
    ))
}

#[cfg(test)]
pub(crate) fn pre_spawn(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_input: Option<&Value>,
    active_agents: usize,
    now_ms: u64,
) -> Result<Option<String>> {
    pre_spawn_with_workspace_and_turn(
        state_root,
        runtime_id,
        session_id,
        tool_input,
        RootHookContext::new(Some("/repo"), active_agents, now_ms),
    )
}

#[cfg(test)]
fn pre_spawn_with_workspace(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_input: Option<&Value>,
    hook_workspace_root: Option<&str>,
    active_agents: usize,
    now_ms: u64,
) -> Result<Option<String>> {
    pre_spawn_with_workspace_and_turn(
        state_root,
        runtime_id,
        session_id,
        tool_input,
        RootHookContext::new(hook_workspace_root, active_agents, now_ms),
    )
}

pub(crate) fn pre_spawn_with_workspace_and_turn(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_input: Option<&Value>,
    context: RootHookContext<'_>,
) -> Result<Option<String>> {
    let RootHookContext {
        workspace_root: hook_workspace_root,
        active_agents,
        now_ms,
    } = context;
    let loaded_rules = rules::load(state_root);
    if let Some(warning) = &loaded_rules.warning {
        eprintln!("Codey 子代理规则回退：{warning}");
    }
    let store = LedgerStore::open(state_root, session_id)?;
    let mut ledger = store
        .load(runtime_id, session_id, now_ms)?
        .unwrap_or_else(|| SessionLedger::new(runtime_id, session_id, now_ms));
    let prepared = match prepare_task_capsule(tool_input, hook_workspace_root, &loaded_rules.rules)
    {
        Ok(prepared) => prepared,
        Err(reason) => return Ok(Some(reason)),
    };
    if ledger.issued_task_ids.contains(&prepared.capsule.id) {
        return Ok(Some(duplicate_task_id_denial(
            &ledger,
            &prepared.capsule.id,
        )));
    }
    if let Some(reason) = ledger_capacity_denial(&ledger) {
        return Ok(Some(reason));
    }
    if let Some(conflict) = resource_conflict(&prepared, &ledger) {
        return Ok(Some(conflict));
    }
    if let Some(conflict) =
        resource_conflict_in_other_sessions(state_root, runtime_id, &store.ledger_path, &prepared)?
    {
        return Ok(Some(conflict));
    }
    if let Some(reason) = concurrency_denial(&ledger, &prepared, active_agents) {
        return Ok(Some(reason));
    }

    ledger.issued_task_ids.insert(prepared.capsule.id.clone());
    let trace = prepared.trace.clone();
    let task_id = prepared.capsule.id.clone();
    let role = prepared.role.clone();
    let fencing_token = ledger.next_fencing_token;
    ledger.next_fencing_token = ledger.next_fencing_token.saturating_add(1);
    let attempt_id = hash_component(&format!(
        "{}:{}:{}:{}",
        hash_component(runtime_id),
        hash_component(session_id),
        task_id,
        fencing_token
    ));
    let policy_revision = loaded_rules.rules.revision;
    let runtime_generation = ledger.runtime_generation;
    let origin_runtime_id_hash = ledger.runtime_id_hash.clone();
    ledger.reservations.insert(
        prepared.capsule.id.clone(),
        Reservation {
            task_id: prepared.capsule.id,
            runtime_generation,
            origin_runtime_id_hash,
            role: prepared.role,
            write_capable: prepared.policy.access == RoleAccess::Write,
            workspace_root: prepared.workspace_root,
            state: ReservationState::Pending,
            outcome: ExecutionOutcome::Unknown,
            agent_id_hash: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            trace_id: prepared.trace.trace_id,
            parent_id: prepared.trace.parent_id,
            capabilities: prepared.capabilities,
            started_at_ms: None,
            completed_at_ms: None,
            token_usage: None,
            error_message: None,
            attempt_id: attempt_id.clone(),
            fencing_token,
            policy_revision,
            fenced_at_ms: None,
            spawn_failed: false,
            pending_init_observed_at_ms: None,
        },
    );
    store.save(&mut ledger, now_ms)?;
    let mut event = SubagentTraceEvent::new(
        now_ms,
        &trace,
        TraceEventKind::Scheduled,
        ExecutionStatus::Pending,
        runtime_id,
        session_id,
        &task_id,
        None,
        Some(&role),
    );
    event.attributes.insert(
        "rules.source".into(),
        Value::String(format!("{:?}", loaded_rules.source).to_ascii_lowercase()),
    );
    event.attributes.insert(
        "rules.revision".into(),
        Value::Number(loaded_rules.rules.revision.into()),
    );
    event
        .attributes
        .insert("admission.mode".into(), Value::String("native".into()));
    event
        .attributes
        .insert("attempt.id".into(), Value::String(attempt_id));
    event
        .attributes
        .insert("fencing.token".into(), Value::Number(fencing_token.into()));
    TraceRecorder::new(state_root).record_best_effort(&event);
    Ok(None)
}

pub(crate) fn post_spawn(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_input: Option<&Value>,
    tool_response: Option<&Value>,
    now_ms: u64,
) -> Result<()> {
    let task_id = match spawn_task_id(tool_input) {
        Ok(Some(task_id)) => task_id,
        Ok(None) => return Ok(()),
        Err(()) => anyhow::bail!(
            "Codey spawn PostToolUse 门禁：task_name/taskName 别名冲突或类型无效，拒绝把回执关联到任何 reservation"
        ),
    };
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(());
    };
    let Some(reservation) = ledger.reservations.get(task_id) else {
        return Ok(());
    };
    if reservation.state != ReservationState::Pending {
        return Ok(());
    }
    let returned_agent_id =
        tool_response.and_then(|response| extract_spawn_binding_identifier(response, task_id));
    if let Some(agent_id) = returned_agent_id.as_deref() {
        let mut conflicts = identity_task_candidates(&ledger, agent_id);
        conflicts.remove(task_id);
        if !conflicts.is_empty() {
            conflicts.insert(task_id.to_string());
            let reason = format!(
                "spawn 回执 agent_id 与其他 attempt 冲突；关联任务：{}",
                conflicts.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            fence_identity_conflict(&mut ledger, &conflicts, now_ms, &reason);
            store.save(&mut ledger, now_ms)?;
            anyhow::bail!(
                "{AGENT_ID_COLLISION_ERROR_CODE}: {reason}。所有相关活动 attempt 已被 fence，禁止复用该身份"
            );
        }
    }
    let reservation = ledger
        .reservations
        .get_mut(task_id)
        .expect("reservation checked above");
    let trace = reservation_trace(reservation);
    let role = reservation.role.clone();
    let mut trace_event = None;
    if returned_agent_id.is_none() && tool_response.is_some_and(response_is_explicit_failure) {
        reservation.state = ReservationState::Terminal;
        reservation.outcome = ExecutionOutcome::Failed;
        reservation.spawn_failed = true;
        reservation.pending_init_observed_at_ms = None;
        reservation.fenced_at_ms = Some(now_ms);
        reservation.updated_at_ms = now_ms;
        reservation.completed_at_ms = Some(now_ms);
        reservation.token_usage = telemetry::extract_token_usage(tool_response);
        reservation.error_message = Some("spawn tool reported failure".to_string());
        let mut event = SubagentTraceEvent::new(
            now_ms,
            &trace,
            TraceEventKind::Failed,
            ExecutionStatus::Failed,
            runtime_id,
            session_id,
            task_id,
            None,
            Some(&role),
        );
        event.latency_ms = Some(now_ms.saturating_sub(reservation.created_at_ms));
        event.usage = reservation.token_usage.clone();
        event.error_code = Some("spawn_failed".into());
        event.error_message = reservation.error_message.clone();
        trace_event = Some(event);
    } else if let Some(agent_id) = returned_agent_id.as_deref() {
        reservation.state = ReservationState::Running;
        reservation.outcome = ExecutionOutcome::Unknown;
        reservation.pending_init_observed_at_ms = None;
        reservation.updated_at_ms = now_ms;
        reservation.started_at_ms = Some(now_ms);
        reservation.agent_id_hash = Some(hash_component(agent_id));
        let mut event = SubagentTraceEvent::new(
            now_ms,
            &trace,
            TraceEventKind::Started,
            ExecutionStatus::Running,
            runtime_id,
            session_id,
            task_id,
            Some(agent_id),
            Some(&role),
        );
        event.latency_ms = Some(now_ms.saturating_sub(reservation.created_at_ms));
        trace_event = Some(event);
    } else {
        // 没有明确失败，也没有可绑定的代理 ID，只能确认工具调用已经返回，不能确认
        // 子代理是否真正创建。保留 Pending，等待生命周期事件或完整状态快照对账。
        reservation.updated_at_ms = now_ms;
    }
    store.save(&mut ledger, now_ms)?;
    if let Some(event) = trace_event {
        TraceRecorder::new(state_root).record_best_effort(&event);
    }
    Ok(())
}

pub(crate) fn pre_followup_task(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_input: Option<&Value>,
    now_ms: u64,
) -> Result<Option<String>> {
    let Some(target) = followup_task_target(tool_input) else {
        return Ok(Some(format!(
            "{FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT_ERROR_CODE}: Codey 生命周期门禁：`agents.followup_task` 缺少非空 target，已在唤醒子代理前拒绝。不要重试本次调用；请修正目标，或由主代理接管。"
        )));
    };
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(Some(followup_without_active_attempt_denial(
            target,
            "当前会话没有可验证的活动委派账本",
        )));
    };
    let Some(task_id) = unique_task_for_identifier(&ledger, target)? else {
        return Ok(Some(followup_without_active_attempt_denial(
            target,
            "target 无法匹配当前账本中的 reservation",
        )));
    };
    let reservation = &ledger.reservations[&task_id];
    if reservation.state == ReservationState::Running
        && reservation.agent_id_hash.is_some()
        && reservation.fenced_at_ms.is_none()
        && !reservation.spawn_failed
    {
        return Ok(None);
    }
    if reservation.state == ReservationState::Pending && reservation.fenced_at_ms.is_none() {
        return Ok(Some(format!(
            "{FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT_ERROR_CODE}: Codey 生命周期门禁：目标 `{target}` 的 attempt `{}` 仍为 pending，尚未绑定 agent_id；已在唤醒子代理前拒绝。不要重试 `followup_task`，请先调用一次不带筛选的 `agents.list_agents` 对账并继续等待。若快照明确没有匹配代理，由主代理接管；只有范围实质改变且仍值得委派时，才使用全新的 task_name 调用 `agents.spawn_agent`。",
            reservation.attempt_id
        )));
    }
    Ok(Some(followup_without_active_attempt_denial(
        target,
        &format!(
            "匹配 attempt `{}` 已不可恢复（state={:?}, outcome={:?}）",
            reservation.attempt_id, reservation.state, reservation.outcome
        ),
    )))
}

fn followup_without_active_attempt_denial(target: &str, detail: &str) -> String {
    format!(
        "{FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT_ERROR_CODE}: Codey 生命周期门禁：`agents.followup_task` 只能用于当前会话中仍为 running、已绑定 agent_id 且未被 fence 的 attempt；目标 `{target}` 不满足条件（{detail}），已在唤醒子代理前拒绝。不要重试 `followup_task`，也不要等待旧 canonical task 自行恢复。若仍有独立工作，使用全新的 task_name 调用 `agents.spawn_agent`；否则由主代理立即接管。"
    )
}

fn duplicate_task_id_denial(ledger: &SessionLedger, task_id: &str) -> String {
    let prefix = format!(
        "{DUPLICATE_TASK_ID_ERROR_CODE}: Codey 自适应委派门禁：任务 ID `{task_id}` 已在本轮编排账本中，禁止重复派生。"
    );
    match ledger.reservations.get(task_id) {
        Some(reservation) if reservation.state == ReservationState::Pending => format!(
            "{prefix} 账本状态为 `pending`，上次派生结果尚未确认。不要重发旧 ID，也不要把本次拒绝当作完成后立即 Stop；先调用一次不带筛选的 `agents.list_agents` 对账。若找到对应代理，等待其进入终态或消费已有终态结果；若明确没有匹配代理，默认由主代理接管。只有任务范围实质改变且仍值得委派时，才可用全新的 `task_name` 最多重试一次。禁止改走 `functions.exec` 重试派生。"
        ),
        Some(reservation) if reservation.state == ReservationState::Running => format!(
            "{prefix} 账本状态为 `running`，原派生已经建立。不要重发旧 ID，也不要直接 Stop；先调用一次不带筛选的 `agents.list_agents` 对账。若代理仍为 `running`、`pending_init` 或 `interrupted`，继续等待或必要协调；若已终态，消费已有结果；若快照明确没有匹配代理，由主代理接管。禁止改走 `functions.exec` 重试派生。"
        ),
        Some(reservation) if reservation.spawn_failed => format!(
            "{prefix} 账本状态为 `failed`，上次派生已明确失败，成本点虽已退还但尝试次数仍计入。不要再次使用旧 ID，也不要把本次拒绝当作完成后立即 Stop；默认由主代理接管。只有任务范围实质改变且仍值得委派时，才可用全新的 `task_name` 最多重试一次。禁止改走 `functions.exec` 重试派生。"
        ),
        Some(reservation) => format!(
            "{prefix} 原任务已经进入终态或恢复态（outcome={:?}），不得重新派生；请消费已有结果并由根代理完成终局验收。",
            reservation.outcome
        ),
        None => format!(
            "{prefix} 当前无法从兼容账本恢复原 reservation。不要重发旧 ID，也不要立即 Stop；先调用一次不带筛选的 `agents.list_agents` 对账。若找到对应代理，等待或消费其结果；若明确没有匹配代理，默认由主代理接管。只有任务范围实质改变且仍值得委派时，才可用全新的 `task_name` 最多重试一次。"
        ),
    }
}

pub(crate) fn subagent_started_with_context(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    agent_id: &str,
    agent_type: Option<&str>,
    transcript_path: Option<&str>,
    now_ms: u64,
) -> Result<bool> {
    update_reservation_lifecycle(
        state_root,
        runtime_id,
        session_id,
        LifecycleContext {
            agent_id,
            agent_type,
            transcript_path,
            state: ReservationState::Running,
        },
        now_ms,
    )
}

#[cfg(test)]
pub(crate) fn subagent_stopped(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    agent_id: &str,
    now_ms: u64,
) -> Result<()> {
    subagent_stopped_with_context(
        state_root, runtime_id, session_id, agent_id, None, None, now_ms,
    )
}

pub(crate) fn subagent_stopped_with_context(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    agent_id: &str,
    agent_type: Option<&str>,
    transcript_path: Option<&str>,
    now_ms: u64,
) -> Result<()> {
    update_reservation_lifecycle(
        state_root,
        runtime_id,
        session_id,
        LifecycleContext {
            agent_id,
            agent_type,
            transcript_path,
            state: ReservationState::Terminal,
        },
        now_ms,
    )
    .map(|_| ())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AnonymousStopSettlement {
    pub(crate) agent_id_hash: Option<String>,
}

/// Settles an identity-less SubagentStop only when the ledger leaves exactly one
/// safe active candidate. Role information narrows the candidate set when the
/// Hook supplies it; ambiguity deliberately remains fail-closed for later
/// authoritative wait/list reconciliation.
pub(crate) fn settle_unique_anonymous_stop(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    agent_type: Option<&str>,
    now_ms: u64,
) -> Result<Option<AnonymousStopSettlement>> {
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(None);
    };
    let candidates = ledger
        .reservations
        .iter()
        .filter(|(_, reservation)| {
            reservation.state.is_active() && agent_type.is_none_or(|role| reservation.role == role)
        })
        .map(|(task_id, _)| task_id.clone())
        .collect::<Vec<_>>();
    let [task_id] = candidates.as_slice() else {
        return Ok(None);
    };
    let reservation = ledger
        .reservations
        .get_mut(task_id)
        .expect("anonymous stop candidate must exist");
    let agent_id_hash = reservation.agent_id_hash.clone();
    reservation.state = ReservationState::Terminal;
    reservation.outcome = ExecutionOutcome::Unknown;
    reservation.pending_init_observed_at_ms = None;
    reservation.updated_at_ms = now_ms;
    reservation.completed_at_ms = Some(now_ms);
    reservation.fenced_at_ms = Some(now_ms);
    reservation.error_message = None;
    store.save(&mut ledger, now_ms)?;
    Ok(Some(AnonymousStopSettlement { agent_id_hash }))
}

struct LifecycleContext<'a> {
    agent_id: &'a str,
    agent_type: Option<&'a str>,
    transcript_path: Option<&'a str>,
    state: ReservationState,
}

fn update_reservation_lifecycle(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    context: LifecycleContext<'_>,
    now_ms: u64,
) -> Result<bool> {
    let LifecycleContext {
        agent_id,
        agent_type,
        transcript_path,
        state,
    } = context;
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        // Marker-only legacy sessions still need the gate fallback.
        return Ok(true);
    };
    let agent_hash = hash_component(agent_id);
    let mut candidates = identity_task_candidates(&ledger, agent_id);
    if candidates.is_empty()
        && let Some(task_id) = task_id_from_subagent_transcript(
            state_root,
            session_id,
            agent_id,
            agent_type,
            transcript_path,
            &ledger,
        )
    {
        candidates.insert(task_id);
    }
    if candidates.len() > 1 {
        let reason = format!(
            "生命周期标识 `{agent_id}` 同时指向多个 attempt：{}",
            candidates.iter().cloned().collect::<Vec<_>>().join(", ")
        );
        fence_identity_conflict(&mut ledger, &candidates, now_ms, &reason);
        store.save(&mut ledger, now_ms)?;
        anyhow::bail!("{AGENT_ID_COLLISION_ERROR_CODE}: {reason}");
    }
    let Some(task_id) = candidates.into_iter().next() else {
        // A provider-owned Start is still live work even when an older/newer
        // provider shape prevents reservation binding. Keep the marker barrier
        // until its matching Stop or an authoritative full snapshot arrives.
        return Ok(true);
    };
    if let Some(role) = agent_type
        && ledger
            .reservations
            .get(&task_id)
            .is_none_or(|reservation| reservation.role != role)
    {
        let reason = format!("生命周期事件角色 `{role}` 与 attempt `{task_id}` 的绑定角色不一致");
        fence_identity_conflict(
            &mut ledger,
            &BTreeSet::from([task_id.clone()]),
            now_ms,
            &reason,
        );
        store.save(&mut ledger, now_ms)?;
        anyhow::bail!("{AGENT_ID_COLLISION_ERROR_CODE}: {reason}");
    }
    if let Some(bound_hash) = ledger
        .reservations
        .get(&task_id)
        .and_then(|reservation| reservation.agent_id_hash.as_deref())
        && bound_hash != agent_hash
        && !is_provisional_task_binding(bound_hash, &task_id)
    {
        let reason = format!("生命周期 agent_id 与 attempt `{task_id}` 的既有运行时身份不一致");
        fence_identity_conflict(
            &mut ledger,
            &BTreeSet::from([task_id.clone()]),
            now_ms,
            &reason,
        );
        store.save(&mut ledger, now_ms)?;
        anyhow::bail!("{AGENT_ID_COLLISION_ERROR_CODE}: {reason}");
    }
    let mut trace_event = None;
    if let Some(reservation) = ledger.reservations.get_mut(&task_id) {
        if reservation.state == state {
            let should_track = reservation.state.is_active();
            let mut changed = false;
            if reservation.agent_id_hash.as_deref() != Some(agent_hash.as_str()) {
                reservation.agent_id_hash = Some(agent_hash);
                if state == ReservationState::Running {
                    reservation.started_at_ms.get_or_insert(now_ms);
                }
                changed = true;
            }
            if reservation.pending_init_observed_at_ms.take().is_some() {
                changed = true;
            }
            if changed {
                reservation.updated_at_ms = now_ms;
                store.save(&mut ledger, now_ms)?;
            }
            return Ok(should_track);
        }
        if reservation.state.transition_to(state).is_none() {
            return Ok(false);
        }
        reservation.state = state;
        reservation.agent_id_hash = Some(agent_hash);
        reservation.pending_init_observed_at_ms = None;
        reservation.updated_at_ms = now_ms;
        let trace_status = match state {
            ReservationState::Running => {
                reservation.outcome = ExecutionOutcome::Unknown;
                reservation.started_at_ms.get_or_insert(now_ms);
                Some((TraceEventKind::Started, ExecutionStatus::Running))
            }
            ReservationState::Terminal => {
                reservation.outcome = ExecutionOutcome::Unknown;
                reservation.completed_at_ms = Some(now_ms);
                reservation.fenced_at_ms = Some(now_ms);
                reservation.error_message = None;
                // SubagentStop proves only that execution ended. Emitting a
                // failed trace here would double-count the attempt when the
                // subsequent wait/list response supplies its real outcome.
                None
            }
            ReservationState::Failed => {
                reservation.outcome = ExecutionOutcome::Failed;
                reservation.completed_at_ms = Some(now_ms);
                reservation.fenced_at_ms = Some(now_ms);
                Some((TraceEventKind::Failed, ExecutionStatus::Failed))
            }
            ReservationState::Recovered => {
                reservation.outcome = ExecutionOutcome::Lost;
                reservation.completed_at_ms = Some(now_ms);
                reservation.fenced_at_ms = Some(now_ms);
                Some((TraceEventKind::Recovered, ExecutionStatus::Recovered))
            }
            ReservationState::Pending => {
                Some((TraceEventKind::Scheduled, ExecutionStatus::Pending))
            }
        };
        if let Some((event_kind, status)) = trace_status {
            let trace = reservation_trace(reservation);
            let mut event = SubagentTraceEvent::new(
                now_ms,
                &trace,
                event_kind,
                status,
                runtime_id,
                session_id,
                &task_id,
                Some(agent_id),
                Some(&reservation.role),
            );
            event.latency_ms = Some(now_ms.saturating_sub(reservation.created_at_ms));
            event.usage = reservation.token_usage.clone();
            event.attributes.insert(
                "execution.outcome".into(),
                Value::String(format!("{:?}", reservation.outcome).to_ascii_lowercase()),
            );
            trace_event = Some(event);
        }
    }
    store.save(&mut ledger, now_ms)?;
    if let Some(event) = trace_event {
        TraceRecorder::new(state_root).record_best_effort(&event);
    }
    Ok(true)
}

pub(crate) fn observe_status_response(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_response: Option<&Value>,
    all_terminal: bool,
    now_ms: u64,
) -> Result<Vec<String>> {
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(Vec::new());
    };
    let mut terminal_tasks = BTreeMap::new();
    if let Some(response) = tool_response
        && let Err(error) =
            collect_terminal_task_outcomes(response, &mut ledger, &mut terminal_tasks, now_ms)
    {
        store.save(&mut ledger, now_ms)?;
        return Err(error);
    }
    if all_terminal {
        for (task_id, reservation) in &ledger.reservations {
            if reservation.state.is_active() {
                terminal_tasks
                    .entry(task_id.clone())
                    .or_insert(ExecutionOutcome::Lost);
            }
        }
    }
    let mut changed = false;
    let mut agent_id_hashes = Vec::new();
    let usage = telemetry::extract_token_usage(tool_response);
    let mut trace_events = Vec::new();
    for (task_id, outcome) in terminal_tasks {
        let Some(reservation) = ledger.reservations.get_mut(&task_id) else {
            continue;
        };
        let transitions_to_terminal = reservation.state != ReservationState::Terminal
            && reservation
                .state
                .transition_to(ReservationState::Terminal)
                .is_some();
        let refines_lifecycle_stop = reservation.state == ReservationState::Terminal
            && reservation.outcome == ExecutionOutcome::Unknown;
        if transitions_to_terminal || refines_lifecycle_stop {
            if let Some(agent_id_hash) = reservation.agent_id_hash.take() {
                agent_id_hashes.push(agent_id_hash);
            }
            reservation.state = ReservationState::Terminal;
            reservation.outcome = outcome;
            reservation.fenced_at_ms.get_or_insert(now_ms);
            reservation.pending_init_observed_at_ms = None;
            reservation.updated_at_ms = now_ms;
            reservation.completed_at_ms.get_or_insert(now_ms);
            reservation.error_message = match outcome {
                ExecutionOutcome::Succeeded => None,
                ExecutionOutcome::Failed => {
                    Some("authoritative agent status reported failure".into())
                }
                ExecutionOutcome::TimedOut => Some("authoritative agent status timed out".into()),
                ExecutionOutcome::Lost => Some(
                    "authoritative status snapshot settled the attempt without a successful result"
                        .into(),
                ),
                ExecutionOutcome::Unknown => Some("terminal outcome was not recognized".into()),
            };
            if usage.is_some() {
                reservation.token_usage = usage.clone();
            }
            let trace = reservation_trace(reservation);
            let success = outcome.is_success();
            let mut event = SubagentTraceEvent::new(
                now_ms,
                &trace,
                if success {
                    TraceEventKind::Completed
                } else {
                    TraceEventKind::Failed
                },
                if success {
                    ExecutionStatus::Succeeded
                } else {
                    ExecutionStatus::Failed
                },
                runtime_id,
                session_id,
                &task_id,
                None,
                Some(&reservation.role),
            );
            event.latency_ms = Some(now_ms.saturating_sub(reservation.created_at_ms));
            event.usage = reservation.token_usage.clone();
            event.attributes.insert(
                "execution.outcome".into(),
                Value::String(format!("{outcome:?}").to_ascii_lowercase()),
            );
            if !success {
                event.error_code = Some(
                    match outcome {
                        ExecutionOutcome::Failed => "agent_failed",
                        ExecutionOutcome::TimedOut => "agent_timed_out",
                        ExecutionOutcome::Lost => "agent_lost",
                        ExecutionOutcome::Unknown => "unknown_terminal_outcome",
                        ExecutionOutcome::Succeeded => unreachable!(),
                    }
                    .into(),
                );
                event.error_message = reservation.error_message.clone();
            }
            trace_events.push(event);
            changed = true;
        }
    }
    if changed {
        store.save(&mut ledger, now_ms)?;
        let recorder = TraceRecorder::new(state_root);
        for event in &trace_events {
            recorder.record_best_effort(event);
        }
    }
    agent_id_hashes.sort();
    agent_id_hashes.dedup();
    Ok(agent_id_hashes)
}

/// Reconciles a full provider list snapshot into reservation-local PendingInit
/// observations and recovers only the attempts whose own grace period elapsed.
/// A live sibling never clears or restarts another reservation's timer.
pub(crate) fn reconcile_pending_init_status_response(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_response: Option<&Value>,
    now_ms: u64,
    grace_ms: u64,
) -> Result<Option<Vec<String>>> {
    let mut observations = Vec::new();
    if let Some(response) = tool_response {
        crate::subagent::protocol::collect_agent_status_observations(response, &mut observations);
    }
    reconcile_pending_init_observations(
        state_root,
        runtime_id,
        session_id,
        &observations,
        now_ms,
        grace_ms,
    )
}

/// Sweeps persisted reservation-local PendingInit timers without requiring a
/// fresh provider response. Stop uses this so a timer cannot be extended merely
/// because another child remains live or the provider stops returning updates.
pub(crate) fn recover_expired_pending_init_reservations(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
    grace_ms: u64,
) -> Result<Option<Vec<String>>> {
    reconcile_pending_init_observations(state_root, runtime_id, session_id, &[], now_ms, grace_ms)
}

fn reconcile_pending_init_observations(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    observations: &[crate::subagent::protocol::AgentStatusObservation],
    now_ms: u64,
    grace_ms: u64,
) -> Result<Option<Vec<String>>> {
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(None);
    };

    let mut task_states = BTreeMap::<String, AgentState>::new();
    let mut conflicting_states = BTreeSet::new();
    for observation in observations {
        let mut resolved_tasks = BTreeSet::new();
        for identifier in &observation.identifiers {
            let candidates = identity_task_candidates(&ledger, identifier);
            if candidates.len() > 1 {
                let reason = format!(
                    "PendingInit 状态标识 `{identifier}` 同时指向多个 attempt（{}）",
                    candidates.iter().cloned().collect::<Vec<_>>().join(", ")
                );
                fence_identity_conflict(&mut ledger, &candidates, now_ms, &reason);
                store.save(&mut ledger, now_ms)?;
                anyhow::bail!("{AGENT_ID_COLLISION_ERROR_CODE}: {reason}");
            }
            resolved_tasks.extend(candidates);
        }
        if resolved_tasks.len() > 1 {
            let reason = format!(
                "同一 PendingInit 状态条目的身份指向不同 attempt（{}）",
                resolved_tasks
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            fence_identity_conflict(&mut ledger, &resolved_tasks, now_ms, &reason);
            store.save(&mut ledger, now_ms)?;
            anyhow::bail!("{AGENT_ID_COLLISION_ERROR_CODE}: {reason}");
        }
        let Some(task_id) = resolved_tasks.into_iter().next() else {
            continue;
        };
        if task_states
            .get(&task_id)
            .is_some_and(|current| *current != observation.state)
        {
            task_states.remove(&task_id);
            conflicting_states.insert(task_id);
            continue;
        }
        if !conflicting_states.contains(&task_id) {
            task_states.insert(task_id, observation.state);
        }
    }

    let mut changed = false;
    for (task_id, state) in task_states {
        let Some(reservation) = ledger.reservations.get_mut(&task_id) else {
            continue;
        };
        let next_observed_at = if reservation.state.is_active()
            && reservation.fenced_at_ms.is_none()
            && state == AgentState::PendingInit
        {
            Some(reservation.pending_init_observed_at_ms.unwrap_or(now_ms))
        } else {
            None
        };
        if reservation.pending_init_observed_at_ms != next_observed_at {
            reservation.pending_init_observed_at_ms = next_observed_at;
            reservation.updated_at_ms = now_ms;
            changed = true;
        }
    }

    let mut agent_id_hashes = Vec::new();
    let mut trace_events = Vec::new();
    for (task_id, reservation) in &mut ledger.reservations {
        if !reservation.state.is_active() {
            if reservation.pending_init_observed_at_ms.take().is_some() {
                changed = true;
            }
            continue;
        }
        let Some(observed_at_ms) = reservation.pending_init_observed_at_ms else {
            continue;
        };
        if now_ms.saturating_sub(observed_at_ms) < grace_ms {
            continue;
        }

        let trace = reservation_trace(reservation);
        let role = reservation.role.clone();
        if let Some(agent_id_hash) = reservation.agent_id_hash.take() {
            agent_id_hashes.push(agent_id_hash);
        }
        reservation.state = ReservationState::Recovered;
        reservation.outcome = ExecutionOutcome::Lost;
        reservation.pending_init_observed_at_ms = None;
        reservation.updated_at_ms = now_ms;
        reservation.completed_at_ms = Some(now_ms);
        reservation.fenced_at_ms = Some(now_ms);
        reservation.error_message = Some(format!(
            "provider kept this attempt in pending_init for at least {grace_ms} ms"
        ));

        let mut event = SubagentTraceEvent::new(
            now_ms,
            &trace,
            TraceEventKind::Recovered,
            ExecutionStatus::Recovered,
            runtime_id,
            session_id,
            task_id,
            None,
            Some(&role),
        );
        event.latency_ms = Some(now_ms.saturating_sub(reservation.created_at_ms));
        event.error_code = Some("pending_init_grace_elapsed".into());
        event.error_message = reservation.error_message.clone();
        event
            .attributes
            .insert("execution.outcome".into(), Value::String("lost".into()));
        event.attributes.insert(
            "pending_init.observed_at_ms".into(),
            Value::Number(observed_at_ms.into()),
        );
        event.attributes.insert(
            "pending_init.elapsed_ms".into(),
            Value::Number(now_ms.saturating_sub(observed_at_ms).into()),
        );
        trace_events.push(event);
        changed = true;
    }

    if changed {
        store.save(&mut ledger, now_ms)?;
        let recorder = TraceRecorder::new(state_root);
        for event in &trace_events {
            recorder.record_best_effort(event);
        }
    }
    Ok(Some(agent_id_hashes))
}

/// Returns the lifecycle ledger projection when a session ledger exists.
/// Active-marker files remain a migration fallback in the gate, but they are
/// no longer the primary source of truth for newly scheduled work.
pub(crate) fn active_reservation_count(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<Option<usize>> {
    Ok(
        active_reservation_projection(state_root, runtime_id, session_id, now_ms)?
            .map(|(count, _)| count),
    )
}

pub(crate) fn active_reservation_projection(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<Option<(usize, BTreeSet<String>)>> {
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(None);
    };
    let active = ledger
        .reservations
        .values()
        .filter(|reservation| reservation.state.is_active())
        .collect::<Vec<_>>();
    let identities = active
        .iter()
        .filter_map(|reservation| reservation.agent_id_hash.clone())
        .collect();
    Ok(Some((active.len(), identities)))
}

/// Proves that every active attempt is a bound, local-read-only child and that
/// the lifecycle marker identities exactly match the ledger projection. This
/// deliberately accepts only `files.read`: read-only roles that can execute
/// commands or use other capabilities keep the normal global root barrier.
pub(crate) fn verified_local_read_only_active_count(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    active_marker_hashes: &BTreeSet<String>,
    now_ms: u64,
) -> Result<Option<usize>> {
    if active_marker_hashes.is_empty() {
        return Ok(None);
    }
    let loaded_rules = rules::load(state_root);
    if let Some(warning) = &loaded_rules.warning {
        eprintln!("Codey 子代理规则回退：{warning}");
    }
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(None);
    };
    let active = ledger
        .reservations
        .values()
        .filter(|reservation| reservation.state.is_active())
        .collect::<Vec<_>>();
    if active.is_empty() || active.len() != active_marker_hashes.len() {
        return Ok(None);
    }

    let mut bound_agent_hashes = BTreeSet::new();
    for reservation in &active {
        let role_is_read_only = loaded_rules
            .rules
            .role_policy(&reservation.role)
            .is_some_and(|policy| policy.access == RoleAccess::ReadOnly);
        let files_read_only = matches!(
            reservation.capabilities.as_slice(),
            [capability] if capability == "files.read"
        );
        let Some(agent_id_hash) = reservation.agent_id_hash.as_ref() else {
            return Ok(None);
        };
        if reservation.spawn_failed
            || reservation.fenced_at_ms.is_some()
            || reservation.started_at_ms.is_none()
            || reservation.outcome != ExecutionOutcome::Unknown
            || !role_is_read_only
            || reservation.write_capable
            || !files_read_only
            || !bound_agent_hashes.insert(agent_id_hash.clone())
        {
            return Ok(None);
        }
    }
    if &bound_agent_hashes != active_marker_hashes {
        return Ok(None);
    }
    Ok(Some(active.len()))
}

/// Atomically fences every still-active reservation before the gate discards
/// legacy marker files. This keeps the ledger as the authoritative source of
/// truth and prevents a Stop recovery loop from resurrecting stale work.
pub(crate) fn recover_active_reservations(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    reason: &str,
    now_ms: u64,
) -> Result<usize> {
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(0);
    };
    let mut recovered = 0_usize;
    for reservation in ledger.reservations.values_mut() {
        if !reservation.state.is_active() {
            continue;
        }
        reservation.state = ReservationState::Recovered;
        reservation.outcome = ExecutionOutcome::Lost;
        reservation.agent_id_hash = None;
        reservation.pending_init_observed_at_ms = None;
        reservation.updated_at_ms = now_ms;
        reservation.completed_at_ms = Some(now_ms);
        reservation.fenced_at_ms = Some(now_ms);
        reservation.error_message = Some(reason.to_string());
        recovered = recovered.saturating_add(1);
    }
    if recovered > 0 {
        store.save(&mut ledger, now_ms)?;
    }
    Ok(recovered)
}

/// A full provider snapshot with no children is authoritative for a spawn that
/// never produced a binding or lifecycle start. Running/bound attempts remain
/// untouched because an empty snapshot may still be transient for live agents.
pub(crate) fn recover_unstarted_reservations(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<usize> {
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(0);
    };
    let mut recovered = 0_usize;
    for reservation in ledger.reservations.values_mut() {
        if reservation.state != ReservationState::Pending
            || reservation.agent_id_hash.is_some()
            || reservation.started_at_ms.is_some()
        {
            continue;
        }
        reservation.state = ReservationState::Recovered;
        reservation.outcome = ExecutionOutcome::Lost;
        reservation.pending_init_observed_at_ms = None;
        reservation.updated_at_ms = now_ms;
        reservation.completed_at_ms = Some(now_ms);
        reservation.fenced_at_ms = Some(now_ms);
        reservation.error_message =
            Some("full agent list reported no child for the pending spawn".into());
        recovered = recovered.saturating_add(1);
    }
    if recovered > 0 {
        store.save(&mut ledger, now_ms)?;
    }
    Ok(recovered)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InterruptSettlement {
    /// Hash used by the legacy active-marker filename. The ledger remains the
    /// source of truth, but returning it lets the gate remove the migration
    /// fallback without retaining a raw provider identifier.
    pub(crate) agent_id_hash: Option<String>,
}

/// Applies a provider-owned interrupt acknowledgement only after every identity
/// in that acknowledgement resolves to the exact reservation requested by the
/// root. A live/pending acknowledgement means the root abandoned the task; a
/// target-specific prior terminal outcome instead settles the attempt with that
/// authoritative result.
pub(crate) fn settle_interrupt_acknowledgement(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_input: Option<&Value>,
    acknowledgement: &InterruptAcknowledgement,
    now_ms: u64,
) -> Result<Option<InterruptSettlement>> {
    let Some(target) = interrupt_task_target(tool_input) else {
        return Ok(None);
    };
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(mut ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(None);
    };
    let Some(task_id) = unique_task_for_identifier(&ledger, &target)? else {
        return Ok(None);
    };

    // Missing response identities remain backward compatible. Once the
    // provider supplies one, however, it is authoritative enough that an
    // unknown, ambiguous, or mismatched identity must keep the fence intact.
    for identifier in &acknowledgement.identifiers {
        let Ok(Some(identity_task_id)) = unique_task_for_identifier(&ledger, identifier) else {
            return Ok(None);
        };
        if identity_task_id != task_id {
            return Ok(None);
        }
    }

    let reservation = ledger
        .reservations
        .get_mut(&task_id)
        .expect("resolved reservation must exist");
    let agent_id_hash = reservation.agent_id_hash.clone();
    let refines_unknown_terminal = reservation.state == ReservationState::Terminal
        && reservation.outcome == ExecutionOutcome::Unknown
        && acknowledgement.prior_outcome.is_some();
    if !reservation.state.is_active() && !refines_unknown_terminal {
        return Ok(Some(InterruptSettlement { agent_id_hash }));
    }

    let trace = reservation_trace(reservation);
    let role = reservation.role.clone();
    let prior_outcome = acknowledgement.prior_outcome.map(|outcome| match outcome {
        TerminalOutcome::Succeeded => ExecutionOutcome::Succeeded,
        TerminalOutcome::Failed => ExecutionOutcome::Failed,
        TerminalOutcome::TimedOut => ExecutionOutcome::TimedOut,
        TerminalOutcome::Lost => ExecutionOutcome::Lost,
    });
    let outcome = prior_outcome.unwrap_or(ExecutionOutcome::Lost);
    reservation.state = if prior_outcome.is_some() {
        ReservationState::Terminal
    } else {
        ReservationState::Recovered
    };
    reservation.outcome = outcome;
    reservation.agent_id_hash = None;
    reservation.pending_init_observed_at_ms = None;
    reservation.updated_at_ms = now_ms;
    reservation.completed_at_ms = Some(now_ms);
    reservation.fenced_at_ms = Some(now_ms);
    reservation.error_message = match prior_outcome {
        Some(ExecutionOutcome::Succeeded) => None,
        Some(ExecutionOutcome::Failed) => {
            Some("interrupt acknowledgement reported that the task had already failed".into())
        }
        Some(ExecutionOutcome::TimedOut) => {
            Some("interrupt acknowledgement reported that the task had already timed out".into())
        }
        Some(ExecutionOutcome::Lost) => Some(
            "interrupt acknowledgement reported that the task had already become unavailable"
                .into(),
        ),
        Some(ExecutionOutcome::Unknown) => unreachable!(),
        None => {
            Some("root successfully interrupted and permanently abandoned this task".to_string())
        }
    };
    let error_message = reservation.error_message.clone();
    store.save(&mut ledger, now_ms)?;

    let success = outcome.is_success();
    let mut event = if prior_outcome.is_some() {
        SubagentTraceEvent::new(
            now_ms,
            &trace,
            if success {
                TraceEventKind::Completed
            } else {
                TraceEventKind::Failed
            },
            if success {
                ExecutionStatus::Succeeded
            } else {
                ExecutionStatus::Failed
            },
            runtime_id,
            session_id,
            &task_id,
            Some(&target),
            Some(&role),
        )
    } else {
        SubagentTraceEvent::new(
            now_ms,
            &trace,
            TraceEventKind::Recovered,
            ExecutionStatus::Recovered,
            runtime_id,
            session_id,
            &task_id,
            Some(&target),
            Some(&role),
        )
    };
    event.attributes.insert(
        "execution.outcome".into(),
        Value::String(format!("{outcome:?}").to_ascii_lowercase()),
    );
    if prior_outcome.is_none() {
        event.error_code = Some("root_interrupt_abandoned".into());
        event.error_message = error_message.clone();
    } else if !success {
        event.error_code = Some(
            match outcome {
                ExecutionOutcome::Failed => "agent_failed",
                ExecutionOutcome::TimedOut => "agent_timed_out",
                ExecutionOutcome::Lost => "agent_lost",
                ExecutionOutcome::Unknown | ExecutionOutcome::Succeeded => unreachable!(),
            }
            .into(),
        );
        event.error_message = error_message;
    }
    TraceRecorder::new(state_root).record_best_effort(&event);

    Ok(Some(InterruptSettlement { agent_id_hash }))
}

pub(crate) struct ChildToolContext<'a> {
    pub(crate) agent_id: &'a str,
    pub(crate) agent_type: Option<&'a str>,
    pub(crate) transcript_path: Option<&'a str>,
    pub(crate) tool_name: &'a str,
    pub(crate) tool_input: Option<&'a Value>,
}

pub(crate) fn authorize_child_tool_with_context(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    context: ChildToolContext<'_>,
    now_ms: u64,
) -> Result<Option<String>> {
    let ChildToolContext {
        agent_id,
        agent_type,
        transcript_path,
        tool_name,
        tool_input,
    } = context;
    let loaded_rules = rules::load(state_root);
    if let Some(warning) = &loaded_rules.warning {
        eprintln!("Codey 子代理规则回退：{warning}");
    }
    let tool_class = rules::classify_tool(tool_name);
    let store = LedgerStore::open(state_root, session_id)?;
    let mut ledger = store.load(runtime_id, session_id, now_ms)?;
    let agent_hash = hash_component(agent_id);
    let mut bound_task = None;
    if let Some(current) = ledger.as_ref() {
        let candidates = identity_task_candidates(current, agent_id);
        if candidates.len() > 1 {
            let reason = format!(
                "{AGENT_ID_COLLISION_ERROR_CODE}: child 标识 `{agent_id}` 同时指向多个 attempt（{}）；相关活动权限已被 fence",
                candidates.iter().cloned().collect::<Vec<_>>().join(", ")
            );
            if let Some(current) = ledger.as_mut() {
                fence_identity_conflict(current, &candidates, now_ms, &reason);
                store.save(current, now_ms)?;
            }
            return Ok(Some(reason));
        }
        bound_task = candidates.into_iter().next();
        if bound_task.is_none() {
            bound_task = task_id_from_subagent_transcript(
                state_root,
                session_id,
                agent_id,
                agent_type,
                transcript_path,
                current,
            );
        }
    }
    if let (Some(expected_role), Some(task_id), Some(current)) =
        (agent_type, bound_task.as_deref(), ledger.as_ref())
        && current
            .reservations
            .get(task_id)
            .is_some_and(|reservation| reservation.role != expected_role)
    {
        let reason = format!(
            "{AGENT_ID_COLLISION_ERROR_CODE}: child 上报角色 `{expected_role}` 与 attempt `{task_id}` 的绑定角色不一致；该 attempt 已被 fence。"
        );
        if let Some(current) = ledger.as_mut() {
            fence_identity_conflict(
                current,
                &BTreeSet::from([task_id.to_string()]),
                now_ms,
                &reason,
            );
            store.save(current, now_ms)?;
        }
        return Ok(Some(reason));
    }
    if let Some(task_id) = bound_task.as_deref()
        && let Some(current) = ledger.as_mut()
    {
        let conflicting_binding = current
            .reservations
            .get(task_id)
            .and_then(|reservation| reservation.agent_id_hash.as_deref())
            .is_some_and(|bound_hash| {
                bound_hash != agent_hash && !is_provisional_task_binding(bound_hash, task_id)
            });
        if conflicting_binding {
            let reason = format!(
                "{AGENT_ID_COLLISION_ERROR_CODE}: child agent_id 与 attempt `{task_id}` 的既有运行时身份不一致；该 attempt 已被 fence"
            );
            fence_identity_conflict(
                current,
                &BTreeSet::from([task_id.to_string()]),
                now_ms,
                &reason,
            );
            store.save(current, now_ms)?;
            return Ok(Some(reason));
        }
        if let Some(reservation) = current.reservations.get_mut(task_id)
            && reservation.state.is_active()
            && reservation.fenced_at_ms.is_none()
            && reservation.agent_id_hash.as_deref() != Some(agent_hash.as_str())
        {
            if reservation.state == ReservationState::Pending {
                reservation.state = ReservationState::Running;
            }
            reservation.agent_id_hash = Some(agent_hash.clone());
            reservation.pending_init_observed_at_ms = None;
            reservation.started_at_ms.get_or_insert(now_ms);
            reservation.updated_at_ms = now_ms;
            store.save(current, now_ms)?;
        }
    }
    let bound_reservation = bound_task.as_ref().and_then(|task_id| {
        ledger
            .as_ref()
            .and_then(|ledger| ledger.reservations.get(task_id))
    });
    let role = bound_reservation.map(|reservation| reservation.role.as_str());
    let decision = loaded_rules.rules.evaluate(&RuleContext {
        actor: RuleActor::Child,
        role,
        tool_name,
        tool_class,
    });
    let capability_denial = match tool_class {
        ToolClass::Read => match bound_reservation {
            None => Some(format!(
                "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 资源门禁：当前 child 无法通过派生回执或生命周期 transcript 与有效活动 attempt 安全关联，禁止执行读取工具。请停止本次调用并把错误码返回主代理；不要猜测 task 归属绕过身份绑定。"
            )),
            Some(reservation)
                if !reservation.state.is_active() || reservation.fenced_at_ms.is_some() =>
            {
                Some(format!(
                    "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 资源门禁：attempt `{}` 已终态、过期或被 fence，禁止继续读取。",
                    reservation.attempt_id
                ))
            }
            Some(reservation) if !reservation_declares_read(reservation) => Some(format!(
                "Codey 能力门禁：attempt `{}` 未声明 `files.read` capability，禁止读取工具 `{tool_name}`。",
                reservation.attempt_id
            )),
            Some(_) => None,
        },
        ToolClass::Visual => match bound_reservation {
            None => Some(format!(
                "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 视觉门禁：当前 child 未绑定有效 attempt，禁止视觉工具 `{tool_name}`。请停止调用并把错误码返回主代理。"
            )),
            Some(reservation)
                if !reservation.state.is_active() || reservation.fenced_at_ms.is_some() =>
            {
                Some(format!(
                    "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 视觉门禁：attempt `{}` 已终态、过期或被 fence，禁止继续使用视觉工具。",
                    reservation.attempt_id
                ))
            }
            Some(reservation) if !reservation_declares_visual(reservation) => Some(format!(
                "Codey 视觉门禁：attempt `{}` 未声明 `visual.inspect` capability，禁止工具 `{tool_name}`。请改派视觉角色或由主代理接管。",
                reservation.attempt_id
            )),
            Some(_) => None,
        },
        ToolClass::Command => match bound_reservation {
            None => Some(format!(
                "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 能力门禁：当前 child 没有由派生结果或生命周期事件绑定的有效 attempt，禁止执行命令。不要重试命令或等待门禁自行恢复；请立即把该错误码返回主代理，由主代理使用全新的 task_name 重新派生或直接接管。"
            )),
            Some(reservation)
                if !reservation.state.is_active() || reservation.fenced_at_ms.is_some() =>
            {
                Some(format!(
                    "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 能力门禁：attempt `{}` 已终态、过期或被 fence，禁止继续执行命令。不要重试或等待恢复；请立即把该错误码返回主代理。",
                    reservation.attempt_id
                ))
            }
            Some(reservation) if !reservation_declares_command(reservation) => Some(format!(
                "Codey 能力门禁：attempt `{}` 未声明 command.execute capability，禁止工具 `{tool_name}`。读取被拒绝时不得用 Bash 回退；应由根代理修正契约或直接接管。",
                reservation.attempt_id
            )),
            Some(_) => None,
        },
        ToolClass::Network => match bound_reservation {
            None => Some(format!(
                "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 网络门禁：当前 child 未绑定有效 attempt，禁止执行网络工具 `{tool_name}`。请停止本次调用并把错误码返回主代理。"
            )),
            Some(reservation)
                if !reservation.state.is_active() || reservation.fenced_at_ms.is_some() =>
            {
                Some(format!(
                    "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 网络门禁：attempt `{}` 已终态、过期或被 fence，禁止继续访问网络。",
                    reservation.attempt_id
                ))
            }
            Some(reservation) if !reservation_declares_read(reservation) => Some(format!(
                "Codey 网络门禁：attempt `{}` 未声明 `files.read` capability，禁止网络读取工具 `{tool_name}`。",
                reservation.attempt_id
            )),
            Some(_) => None,
        },
        ToolClass::Write => match bound_reservation {
            None => Some(format!(
                "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 能力/资源门禁：当前 child 未绑定有效 attempt，禁止执行写入工具。不要重试写入或等待门禁自行恢复；请立即把该错误码返回主代理，由主代理使用全新的 task_name 重新派生或直接接管。"
            )),
            Some(reservation)
                if !reservation.state.is_active() || reservation.fenced_at_ms.is_some() =>
            {
                Some(format!(
                    "{UNBOUND_ATTEMPT_ERROR_CODE}: Codey 能力/资源门禁：attempt `{}` 已终态、过期或被 fence，禁止恢复写权限。不要重试或等待恢复；请立即把该错误码返回主代理。",
                    reservation.attempt_id
                ))
            }
            Some(reservation) if !reservation_declares_write(reservation) => Some(format!(
                "Codey 能力门禁：attempt `{}` 当前不具备写入权限，工具 `{tool_name}` 未执行。请停止在本子代理内重试写入，把修改建议和证据返回主代理，由主代理完成写入。",
                reservation.attempt_id
            )),
            Some(_) => None,
        },
        ToolClass::Collaboration if !safe_child_reporting_tool(tool_name, tool_input) => {
            Some(
                "Codey 子代理协作门禁：child 只能使用 `agents.send_message` 向 `/root` 回报；不得查看、等待、中断、追派或向其他代理发送消息。"
                    .to_string(),
            )
        }
        _ => None,
    };
    let trace = bound_reservation
        .map(reservation_trace)
        .unwrap_or_else(|| TraceContext::new(None));
    let audit_task = bound_task.as_deref().unwrap_or("unbound");
    let mut audit = SubagentTraceEvent::new(
        now_ms,
        &trace,
        TraceEventKind::RuleEvaluated,
        if decision.effect == RuleEffect::Allow && capability_denial.is_none() {
            ExecutionStatus::Running
        } else {
            ExecutionStatus::Failed
        },
        runtime_id,
        session_id,
        audit_task,
        Some(agent_id),
        role,
    );
    audit
        .attributes
        .insert("rule.id".into(), Value::String(decision.rule_id.clone()));
    audit.attributes.insert(
        "rule.priority".into(),
        Value::Number(decision.priority.into()),
    );
    audit.attributes.insert(
        "rule.effect".into(),
        Value::String(format!("{:?}", decision.effect).to_ascii_lowercase()),
    );
    audit.attributes.insert(
        "rule.conflicts".into(),
        Value::Array(
            decision
                .conflicts
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    audit.attributes.insert(
        "tool.class".into(),
        Value::String(format!("{:?}", tool_class).to_ascii_lowercase()),
    );
    audit.attributes.insert(
        "rules.revision".into(),
        Value::Number(loaded_rules.rules.revision.into()),
    );
    if let Some(reservation) = bound_reservation {
        audit.attributes.insert(
            "reservation.policy_revision".into(),
            Value::Number(reservation.policy_revision.into()),
        );
        audit.attributes.insert(
            "fencing.token".into(),
            Value::Number(reservation.fencing_token.into()),
        );
        audit.attributes.insert(
            "capabilities".into(),
            Value::Array(
                reservation
                    .capabilities
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if capability_denial.is_some() || decision.effect == RuleEffect::Deny {
        TraceRecorder::new(state_root).record_best_effort(&audit);
    }
    if let Some(reason) = capability_denial {
        return Ok(Some(reason));
    }
    if decision.effect == RuleEffect::Deny {
        return Ok(Some(format!(
            "Codey 规则门禁：规则 `{}`（优先级 {}）拒绝工具 `{tool_name}`：{}",
            decision.rule_id, decision.priority, decision.explanation
        )));
    }
    Ok(None)
}

pub(crate) fn settle_turn(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<()> {
    let store = LedgerStore::open(state_root, session_id)?;
    let Some(ledger) = store.load(runtime_id, session_id, now_ms)? else {
        return Ok(());
    };
    anyhow::ensure!(
        !ledger_has_outstanding(&ledger),
        "Codey 子代理仍有未结算的活动 attempt"
    );
    store.remove()
}

pub(crate) fn end_session(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<()> {
    LedgerStore::open(state_root, session_id)?
        .remove_for_session_end(runtime_id, session_id, now_ms)
}

fn ledger_has_outstanding(ledger: &SessionLedger) -> bool {
    ledger
        .reservations
        .values()
        .any(|reservation| !reservation.state.is_settled())
}

fn reservation_declares_command(reservation: &Reservation) -> bool {
    reservation
        .capabilities
        .iter()
        .any(|capability| capability == "command.execute")
}

fn reservation_declares_read(reservation: &Reservation) -> bool {
    reservation
        .capabilities
        .iter()
        .any(|capability| capability == "files.read")
}

fn reservation_declares_visual(reservation: &Reservation) -> bool {
    reservation
        .capabilities
        .iter()
        .any(|capability| capability == "visual.inspect")
}

fn reservation_declares_write(reservation: &Reservation) -> bool {
    reservation.write_capable
        && reservation
            .capabilities
            .iter()
            .any(|capability| capability == "workspace.write")
}

fn reservation_trace(reservation: &Reservation) -> TraceContext {
    TraceContext {
        trace_id: if reservation.trace_id.is_empty() {
            hash_component(&reservation.task_id)
        } else {
            reservation.trace_id.clone()
        },
        parent_id: reservation.parent_id.clone(),
    }
}

fn resource_conflict(prepared: &PreparedContract, ledger: &SessionLedger) -> Option<String> {
    for existing in ledger
        .reservations
        .values()
        .filter(|reservation| !reservation.spawn_failed && reservation.state.is_active())
    {
        if let Some(conflict) = reservation_resource_conflict(prepared, existing) {
            return Some(conflict);
        }
    }
    None
}

fn resource_conflict_in_other_sessions(
    state_root: &Path,
    runtime_id: &str,
    current_ledger_path: &Path,
    prepared: &PreparedContract,
) -> Result<Option<String>> {
    let runtime_id_hash = hash_component(runtime_id);
    for entry in fs::read_dir(state_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let ledger_path = entry.path().join(LEDGER_FILE);
        if ledger_path == current_ledger_path {
            continue;
        }
        let bytes = match crate::fs_util::read_bounded(&ledger_path, MAX_LEDGER_BYTES) {
            Ok(bytes) => bytes,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("读取跨会话子代理账本失败：{}", ledger_path.display())
                });
            }
        };
        let ledger: SessionLedger = serde_json::from_slice(&bytes)
            .with_context(|| format!("解析跨会话子代理账本失败：{}", ledger_path.display()))?;
        if ledger.runtime_id_hash != runtime_id_hash {
            continue;
        }
        for existing in ledger
            .reservations
            .values()
            .filter(|reservation| !reservation.spawn_failed && reservation.state.is_active())
        {
            if let Some(conflict) = reservation_resource_conflict(prepared, existing) {
                return Ok(Some(conflict));
            }
        }
    }
    Ok(None)
}

fn reservation_resource_conflict(
    prepared: &PreparedContract,
    existing: &Reservation,
) -> Option<String> {
    if prepared.policy.access != RoleAccess::Write && !existing.write_capable {
        return None;
    }
    let overlaps = match (&prepared.workspace_root, &existing.workspace_root) {
        (Some(left), Some(right)) => paths_overlap(left, right),
        _ => true,
    };
    overlaps.then(|| {
        format!(
            "Codey 能力/资源冲突门禁：任务 `{}` 与活动任务 `{}` 共享工作区且至少一方可写；请串行执行。",
            prepared.capsule.id, existing.task_id
        )
    })
}

fn paths_overlap(left: &str, right: &str) -> bool {
    path_is_within(left, right) || path_is_within(right, left)
}

fn path_is_within(path: &str, parent: &str) -> bool {
    path == parent
        || parent.ends_with('/') && path.starts_with(parent)
        || path
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(crate) fn safe_child_reporting_tool(tool_name: &str, tool_input: Option<&Value>) -> bool {
    if rules::normalize_tool_name(tool_name) != "send_message" {
        return false;
    }
    let target = tool_input.and_then(Value::as_object).and_then(|values| {
        values.iter().find_map(|(key, value)| {
            (normalized_identifier(key) == "target")
                .then(|| value.as_str().map(str::trim))
                .flatten()
        })
    });
    target.is_some_and(|target| matches!(target.trim_end_matches('/'), "root" | "/root"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn spawn_input(task: &str, role: &str) -> Value {
        json!({
            "task_name": task,
            "agent_type": role,
            "message": "Do the bounded task."
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_and_bind(
        state_root: &Path,
        session_id: &str,
        task_id: &str,
        role: &str,
        agent_id: &str,
        workspace: &str,
        active_agents: usize,
        now_ms: u64,
    ) {
        let input = spawn_input(task_id, role);
        assert_eq!(
            pre_spawn_with_workspace(
                state_root,
                "runtime-a",
                session_id,
                Some(&input),
                Some(workspace),
                active_agents,
                now_ms,
            )
            .unwrap(),
            None
        );
        post_spawn(
            state_root,
            "runtime-a",
            session_id,
            Some(&input),
            Some(&json!({ "agent_id": agent_id })),
            now_ms + 1,
        )
        .unwrap();
    }

    #[test]
    fn native_capsule_assigns_role_capabilities() {
        let temp = tempdir().unwrap();
        let rules = rules::load(temp.path()).rules;
        let readonly = prepare_task_capsule(
            Some(&spawn_input("read_task", "codey_quick_scan")),
            Some("/repo"),
            &rules,
        )
        .unwrap();
        assert_eq!(readonly.capabilities, ["files.read"]);

        let visual = prepare_task_capsule(
            Some(&spawn_input("visual_task", "codey_visual_analysis")),
            Some("/repo"),
            &rules,
        )
        .unwrap();
        assert_eq!(visual.capabilities, ["files.read", "visual.inspect"]);

        let writer = prepare_task_capsule(
            Some(&spawn_input("write_task", "codey_worker")),
            Some("/repo"),
            &rules,
        )
        .unwrap();
        assert_eq!(
            writer.capabilities,
            ["command.execute", "files.read", "workspace.write"]
        );

        let visual_writer = prepare_task_capsule(
            Some(&spawn_input("visual_write_task", "codey_visual_worker")),
            Some("/repo"),
            &rules,
        )
        .unwrap();
        assert_eq!(
            visual_writer.capabilities,
            [
                "command.execute",
                "files.read",
                "workspace.write",
                "visual.inspect"
            ]
        );
    }

    #[test]
    fn visual_tools_require_a_bound_visual_role() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        admit_and_bind(
            root,
            "visual-session",
            "visual_reader",
            "codey_visual_analysis",
            "visual-agent",
            "/repo",
            0,
            10,
        );
        for tool_name in [
            "functions.view_image",
            "mcp__cua_repl__js",
            "mcp__codex_app__open_in_codex",
        ] {
            assert_eq!(
                authorize_child_tool_with_context(
                    root,
                    "runtime-a",
                    "visual-session",
                    ChildToolContext {
                        agent_id: "visual-agent",
                        agent_type: Some("codey_visual_analysis"),
                        transcript_path: None,
                        tool_name,
                        tool_input: None,
                    },
                    20,
                )
                .unwrap(),
                None,
                "{tool_name}"
            );
        }

        admit_and_bind(
            root,
            "scan-session",
            "plain_reader",
            "codey_quick_scan",
            "scan-agent",
            "/repo",
            0,
            30,
        );
        let denied = authorize_child_tool_with_context(
            root,
            "runtime-a",
            "scan-session",
            ChildToolContext {
                agent_id: "scan-agent",
                agent_type: Some("codey_quick_scan"),
                transcript_path: None,
                tool_name: "mcp__cua_repl__js",
                tool_input: None,
            },
            40,
        )
        .unwrap()
        .unwrap();
        assert!(denied.contains("visual.inspect"));
    }

    #[test]
    fn writer_uses_a_workspace_wide_conflict_lock() {
        let temp = tempdir().unwrap();
        assert_eq!(
            pre_spawn_with_workspace(
                temp.path(),
                "runtime-a",
                "session-b",
                Some(&spawn_input("writer_a", "codey_worker")),
                Some("/repo"),
                0,
                10,
            )
            .unwrap(),
            None
        );
        let denial = pre_spawn_with_workspace(
            temp.path(),
            "runtime-a",
            "session-a",
            Some(&spawn_input("writer_b", "codey_worker")),
            Some("/repo"),
            1,
            11,
        )
        .unwrap()
        .unwrap();
        assert!(denial.contains("资源冲突"));
        assert_eq!(
            pre_spawn_with_workspace(
                temp.path(),
                "runtime-a",
                "session-c",
                Some(&spawn_input("writer_c", "codey_worker")),
                Some("/other-repo"),
                0,
                12,
            )
            .unwrap(),
            None
        );
        assert_eq!(
            pre_spawn_with_workspace(
                temp.path(),
                "runtime-b",
                "session-d",
                Some(&spawn_input("writer_d", "codey_worker")),
                Some("/repo"),
                0,
                13,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn terminal_lifecycle_settles_without_a_batch_decision() {
        let temp = tempdir().unwrap();
        let input = spawn_input("read_task", "codey_quick_scan");
        assert_eq!(
            pre_spawn(temp.path(), "runtime-a", "session-a", Some(&input), 0, 10).unwrap(),
            None
        );
        post_spawn(
            temp.path(),
            "runtime-a",
            "session-a",
            Some(&input),
            Some(&json!({ "agent_id": "agent-a" })),
            11,
        )
        .unwrap();
        subagent_stopped(temp.path(), "runtime-a", "session-a", "agent-a", 12).unwrap();
        settle_turn(temp.path(), "runtime-a", "session-a", 13).unwrap();
        assert!(
            LedgerStore::open(temp.path(), "session-a")
                .unwrap()
                .load("runtime-a", "session-a", 14)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn terminal_read_agent_releases_a_concurrency_slot_immediately() {
        let temp = tempdir().unwrap();
        let session = "read-refill";
        for (index, (task, agent)) in [
            ("read_a", "agent-a"),
            ("read_b", "agent-b"),
            ("read_c", "agent-c"),
        ]
        .into_iter()
        .enumerate()
        {
            admit_and_bind(
                temp.path(),
                session,
                task,
                "codey_quick_scan",
                agent,
                "/repo",
                index,
                10 + index as u64 * 10,
            );
        }

        let refill = spawn_input("read_d", "codey_quick_scan");
        let denial = pre_spawn_with_workspace(
            temp.path(),
            "runtime-a",
            session,
            Some(&refill),
            Some("/repo"),
            3,
            40,
        )
        .unwrap()
        .unwrap();
        assert!(denial.contains("并发上限 3"));

        subagent_stopped(temp.path(), "runtime-a", session, "agent-a", 41).unwrap();
        assert_eq!(
            pre_spawn_with_workspace(
                temp.path(),
                "runtime-a",
                session,
                Some(&refill),
                Some("/repo"),
                2,
                42,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn mixed_limit_recomputes_after_the_writer_stops() {
        let temp = tempdir().unwrap();
        let session = "mixed-refill";
        admit_and_bind(
            temp.path(),
            session,
            "writer_a",
            "codey_worker",
            "agent-writer",
            "/write-repo",
            0,
            10,
        );
        admit_and_bind(
            temp.path(),
            session,
            "read_a",
            "codey_quick_scan",
            "agent-reader",
            "/read-repo",
            1,
            20,
        );

        let read_refill = spawn_input("read_b", "codey_quick_scan");
        let denial = pre_spawn_with_workspace(
            temp.path(),
            "runtime-a",
            session,
            Some(&read_refill),
            Some("/read-repo"),
            2,
            30,
        )
        .unwrap()
        .unwrap();
        assert!(denial.contains("并发上限 2"));

        subagent_stopped(temp.path(), "runtime-a", session, "agent-writer", 31).unwrap();
        assert_eq!(
            pre_spawn_with_workspace(
                temp.path(),
                "runtime-a",
                session,
                Some(&read_refill),
                Some("/read-repo"),
                1,
                32,
            )
            .unwrap(),
            None
        );

        let writer_refill = spawn_input("writer_b", "codey_worker");
        let denial = pre_spawn_with_workspace(
            temp.path(),
            "runtime-a",
            session,
            Some(&writer_refill),
            Some("/write-repo"),
            2,
            33,
        )
        .unwrap()
        .unwrap();
        assert!(denial.contains("并发上限 2"));
    }

    #[test]
    fn reporting_is_limited_to_root() {
        assert!(safe_child_reporting_tool(
            "agents.send_message",
            Some(&json!({ "target": "/root" }))
        ));
        assert!(!safe_child_reporting_tool(
            "agents.send_message",
            Some(&json!({ "target": "/root/peer" }))
        ));
    }

    #[test]
    fn coordination_paths_are_normalized() {
        assert_eq!(
            normalize_absolute_path("/repo/src/../tests").unwrap(),
            "/repo/tests"
        );
        assert!(normalize_absolute_path("../repo").is_err());
    }
}
