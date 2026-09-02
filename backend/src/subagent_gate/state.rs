//! Durable per-runtime and per-session state used by the Hook gate.
//!
//! This module owns state files, timestamps, markers, and their validation. It
//! deliberately does not decide whether a Hook event is allowed or denied.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(test)]
use super::current_runtime_id;
use super::{
    ACTIVE_MARKER_SCHEMA_VERSION, HOOK_STATE_LOCK_FILE, PROTOCOL_HEALTH_FILE,
    PROTOCOL_HEALTH_SCHEMA_VERSION, ROOT_TURN_BINDING_FILE, ROOT_TURN_BINDING_SCHEMA_VERSION,
};

pub(super) struct HookStateLock(File);

impl HookStateLock {
    pub(super) fn acquire(state_root: &Path) -> Result<Self> {
        fs::create_dir_all(state_root)?;
        let path = state_root.join(HOOK_STATE_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self(file)),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || (cfg!(windows) && error.raw_os_error() == Some(33)) =>
                {
                    anyhow::ensure!(
                        started.elapsed() < Duration::from_secs(2),
                        "获取 Codex 子代理 Hook 状态锁超时：{}",
                        path.display()
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for HookStateLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ActiveMarker {
    pub(super) schema_version: u32,
    pub(super) runtime_id_hash: String,
    pub(super) started_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ProtocolHealth {
    pub(super) schema_version: u32,
    pub(super) runtime_id_hash: String,
    pub(super) first_issue_at_ms: u64,
    pub(super) last_issue_at_ms: u64,
    #[serde(default)]
    pub(super) missing_agent_id_events: u16,
    #[serde(default)]
    pub(super) unknown_status_responses: u16,
    #[serde(default)]
    pub(super) absolute_stop_timeouts: u16,
    pub(super) last_issue: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RootTurnBinding {
    pub(super) schema_version: u32,
    pub(super) runtime_id_hash: String,
    pub(super) turn_id_hash: String,
    pub(super) bound_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProtocolIssueKind {
    MissingAgentId,
    UnknownStatusResponse,
    AbsoluteStopTimeout,
}

pub(super) fn current_timestamp_millis() -> u64 {
    u64::try_from(crate::fs_util::timestamp_millis()).unwrap_or(u64::MAX)
}

pub(super) fn observe_and_check_elapsed(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    file_name: &str,
    now_ms: u64,
    grace_ms: u64,
) -> Result<bool> {
    let session_dir = session_state_dir(state_root, session_id);
    let path = session_auxiliary_path(&session_dir, runtime_id, file_name);
    match read_observation_timestamp(&path)? {
        Some(observed_at_ms) => {
            let Some(elapsed) = now_ms.checked_sub(observed_at_ms) else {
                write_observation_timestamp(&session_dir, &path, now_ms)?;
                return Ok(false);
            };
            Ok(elapsed >= grace_ms)
        }
        None => {
            write_observation_timestamp(&session_dir, &path, now_ms)?;
            Ok(false)
        }
    }
}

pub(super) fn observation_elapsed_if_present(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    file_name: &str,
    now_ms: u64,
    grace_ms: u64,
) -> Result<bool> {
    let path = session_auxiliary_path(
        &session_state_dir(state_root, session_id),
        runtime_id,
        file_name,
    );
    let Some(observed_at_ms) = read_observation_timestamp(&path)? else {
        return Ok(false);
    };
    let Some(elapsed) = now_ms.checked_sub(observed_at_ms) else {
        let session_dir = session_state_dir(state_root, session_id);
        write_observation_timestamp(&session_dir, &path, now_ms)?;
        return Ok(false);
    };
    Ok(elapsed >= grace_ms)
}

pub(super) fn read_observation_timestamp(path: &Path) -> Result<Option<u64>> {
    match crate::fs_util::read_bounded(path, 64) {
        Ok(bytes) => std::str::from_utf8(&bytes)
            .context("Codex 子代理门禁观察时间不是 UTF-8")?
            .trim()
            .parse::<u64>()
            .map(Some)
            .with_context(|| format!("解析 Codex 子代理门禁观察时间失败：{}", path.display())),
        Err(error) if error_is_not_found(&error) => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("读取 Codex 子代理门禁观察时间失败：{}", path.display())),
    }
}

pub(super) fn write_observation_timestamp(
    session_dir: &Path,
    path: &Path,
    now_ms: u64,
) -> Result<()> {
    fs::create_dir_all(session_dir).with_context(|| {
        format!(
            "创建 Codex 子代理门禁状态目录失败：{}",
            session_dir.display()
        )
    })?;
    crate::fs_util::atomic_write_private(path, format!("{now_ms}\n").as_bytes())
        .with_context(|| format!("替换 Codex 子代理门禁观察状态失败：{}", path.display()))
}

pub(super) fn record_protocol_issue(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    kind: ProtocolIssueKind,
    detail: &str,
    now_ms: u64,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    let path = session_auxiliary_path(&session_dir, runtime_id, PROTOCOL_HEALTH_FILE);
    let runtime_id_hash = hash_component(runtime_id);
    let mut health = read_protocol_health(&path)?.unwrap_or_else(|| ProtocolHealth {
        schema_version: PROTOCOL_HEALTH_SCHEMA_VERSION,
        runtime_id_hash: runtime_id_hash.clone(),
        first_issue_at_ms: now_ms,
        last_issue_at_ms: now_ms,
        missing_agent_id_events: 0,
        unknown_status_responses: 0,
        absolute_stop_timeouts: 0,
        last_issue: detail.to_string(),
    });
    validate_protocol_health(&health, runtime_id)?;
    match kind {
        ProtocolIssueKind::MissingAgentId => {
            health.missing_agent_id_events = health.missing_agent_id_events.saturating_add(1);
        }
        ProtocolIssueKind::UnknownStatusResponse => {
            health.unknown_status_responses = health.unknown_status_responses.saturating_add(1);
        }
        ProtocolIssueKind::AbsoluteStopTimeout => {
            health.absolute_stop_timeouts = health.absolute_stop_timeouts.saturating_add(1);
        }
    }
    health.last_issue_at_ms = now_ms;
    health.last_issue = detail.to_string();
    write_protocol_health(&session_dir, &path, &health)
}

pub(super) fn clear_unknown_status_protocol_issue(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    let path = session_auxiliary_path(&session_dir, runtime_id, PROTOCOL_HEALTH_FILE);
    let Some(mut health) = read_protocol_health(&path)? else {
        return Ok(());
    };
    validate_protocol_health(&health, runtime_id)?;
    if health.unknown_status_responses == 0 {
        return Ok(());
    }
    health.unknown_status_responses = 0;
    health.last_issue_at_ms = now_ms;
    if health.missing_agent_id_events == 0 && health.absolute_stop_timeouts == 0 {
        return remove_session_auxiliary_file(
            state_root,
            runtime_id,
            session_id,
            PROTOCOL_HEALTH_FILE,
        );
    }
    if health.missing_agent_id_events > 0 {
        health.last_issue = "子代理事件曾缺少 agent_id".to_string();
    }
    write_protocol_health(&session_dir, &path, &health)
}

pub(super) fn protocol_issue_reason(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
) -> Result<Option<String>> {
    let path = session_auxiliary_path(
        &session_state_dir(state_root, session_id),
        runtime_id,
        PROTOCOL_HEALTH_FILE,
    );
    let Some(health) = read_protocol_health(&path)? else {
        return Ok(None);
    };
    validate_protocol_health(&health, runtime_id)?;
    let mut issues = Vec::new();
    if health.missing_agent_id_events > 0 {
        issues.push(format!(
            "有 {} 个子代理生命周期事件缺少 agent_id",
            health.missing_agent_id_events
        ));
    }
    if health.unknown_status_responses > 0 {
        issues.push(format!(
            "有 {} 个 wait/list 响应结构无法识别",
            health.unknown_status_responses
        ));
    }
    if health.absolute_stop_timeouts > 0 {
        issues.push(format!(
            "有 {} 次根代理 Stop 按 60 分钟绝对上限放行",
            health.absolute_stop_timeouts
        ));
    }
    if issues.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "{}；最近一次：{}",
            issues.join("，"),
            health.last_issue
        )))
    }
}

pub(super) fn read_protocol_health(path: &Path) -> Result<Option<ProtocolHealth>> {
    let bytes = match crate::fs_util::read_bounded(path, 64 * 1024) {
        Ok(bytes) => bytes,
        Err(error) if error_is_not_found(&error) => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 Codex Hook 协议诊断状态失败：{}", path.display()));
        }
    };
    let health = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Codex Hook 协议诊断状态失败：{}", path.display()))?;
    Ok(Some(health))
}

pub(super) fn validate_protocol_health(health: &ProtocolHealth, runtime_id: &str) -> Result<()> {
    anyhow::ensure!(
        health.schema_version == PROTOCOL_HEALTH_SCHEMA_VERSION
            && health.runtime_id_hash == hash_component(runtime_id),
        "Codex Hook 协议诊断状态与当前运行代次不兼容"
    );
    Ok(())
}

pub(super) fn write_protocol_health(
    session_dir: &Path,
    path: &Path,
    health: &ProtocolHealth,
) -> Result<()> {
    fs::create_dir_all(session_dir).with_context(|| {
        format!(
            "创建 Codex Hook 协议诊断目录失败：{}",
            session_dir.display()
        )
    })?;
    let bytes = serde_json::to_vec(health).context("序列化 Codex Hook 协议诊断状态失败")?;
    crate::fs_util::atomic_write_private(path, &bytes)
        .with_context(|| format!("替换 Codex Hook 协议诊断状态失败：{}", path.display()))
}

pub(super) fn bind_root_turn(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    turn_id: &str,
    now_ms: u64,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    fs::create_dir_all(&session_dir).with_context(|| {
        format!(
            "创建 Codex 根代理 turn 绑定目录失败：{}",
            session_dir.display()
        )
    })?;
    let path = session_auxiliary_path(&session_dir, runtime_id, ROOT_TURN_BINDING_FILE);
    let binding = RootTurnBinding {
        schema_version: ROOT_TURN_BINDING_SCHEMA_VERSION,
        runtime_id_hash: hash_component(runtime_id),
        turn_id_hash: hash_component(turn_id),
        bound_at_ms: now_ms,
    };
    let bytes = serde_json::to_vec(&binding).context("序列化 Codex 根代理 turn 绑定失败")?;
    crate::fs_util::atomic_write_private(&path, &bytes)
        .with_context(|| format!("写入 Codex 根代理 turn 绑定失败：{}", path.display()))
}

pub(super) fn root_turn_matches(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    turn_id: Option<&str>,
) -> Result<bool> {
    let Some(turn_id) = turn_id else {
        return Ok(false);
    };
    let session_dir = session_state_dir(state_root, session_id);
    let path = session_auxiliary_path(&session_dir, runtime_id, ROOT_TURN_BINDING_FILE);
    let bytes = match crate::fs_util::read_bounded(&path, 16 * 1024) {
        Ok(bytes) => bytes,
        Err(error) if error_is_not_found(&error) => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 Codex 根代理 turn 绑定失败：{}", path.display()));
        }
    };
    let binding: RootTurnBinding = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Codex 根代理 turn 绑定失败：{}", path.display()))?;
    anyhow::ensure!(
        binding.schema_version == ROOT_TURN_BINDING_SCHEMA_VERSION,
        "Codex 根代理 turn 绑定版本不受支持：{}",
        path.display()
    );
    anyhow::ensure!(
        binding.runtime_id_hash == hash_component(runtime_id),
        "Codex 根代理 turn 绑定代次不一致：{}",
        path.display()
    );
    Ok(binding.turn_id_hash == hash_component(turn_id))
}

pub(super) fn remove_session_auxiliary_file(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    file_name: &str,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    let path = session_auxiliary_path(&session_dir, runtime_id, file_name);
    match fs::remove_file(&path) {
        Ok(()) => remove_empty_session_dir(&session_dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("移除 Codex 子代理门禁辅助状态失败：{}", path.display())),
    }
}

pub(super) fn create_active_marker(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    agent_id: &str,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    fs::create_dir_all(&session_dir).with_context(|| {
        format!(
            "创建 Codex 子代理门禁状态目录失败：{}",
            session_dir.display()
        )
    })?;
    let marker = agent_marker_path(&session_dir, runtime_id, agent_id);
    let runtime_id_hash = hash_component(runtime_id);
    let state = ActiveMarker {
        schema_version: ACTIVE_MARKER_SCHEMA_VERSION,
        runtime_id_hash,
        started_at_ms: current_timestamp_millis(),
    };
    let bytes = serde_json::to_vec(&state).context("序列化 Codex 子代理门禁状态失败")?;
    crate::fs_util::atomic_write_private(&marker, &bytes)
        .with_context(|| format!("替换 Codex 子代理门禁状态失败：{}", marker.display()))
}

pub(super) fn remove_active_marker(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    agent_id: &str,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    let marker = agent_marker_path(&session_dir, runtime_id, agent_id);
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("移除 Codex 子代理门禁状态失败：{}", marker.display()));
        }
    }
    remove_empty_session_dir(&session_dir)
}

pub(super) fn remove_active_marker_by_hash(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    agent_id_hash: &str,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    let marker = agent_marker_path_from_hash(&session_dir, runtime_id, agent_id_hash);
    match fs::remove_file(&marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("按哈希移除 Codex 子代理门禁状态失败：{}", marker.display())
            });
        }
    }
    remove_empty_session_dir(&session_dir)
}

pub(super) fn remove_empty_session_dir(session_dir: &Path) -> Result<()> {
    match fs::remove_dir(session_dir) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "清理 Codex 子代理门禁状态目录失败：{}",
                    session_dir.display()
                )
            });
        }
    }
    Ok(())
}

pub(super) fn remove_session_state(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
) -> Result<()> {
    let session_dir = session_state_dir(state_root, session_id);
    let entries = match fs::read_dir(&session_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "读取 Codex 子代理门禁会话状态失败：{}",
                    session_dir.display()
                )
            });
        }
    };
    let prefix = runtime_marker_prefix(runtime_id);
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() && runtime_file_has_prefix(&entry.path(), &prefix) {
            fs::remove_file(entry.path()).with_context(|| {
                format!(
                    "清理 Codex 子代理门禁会话状态失败：{}",
                    entry.path().display()
                )
            })?;
        }
    }
    remove_empty_session_dir(&session_dir)
}

#[cfg(test)]
pub(super) fn active_agent_count(state_root: &Path, session_id: &str) -> Result<usize> {
    active_agent_count_for_runtime(state_root, &current_runtime_id(), session_id)
}

pub(super) fn active_agent_count_for_runtime(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
) -> Result<usize> {
    let marker_hashes = active_marker_hashes_for_runtime(state_root, runtime_id, session_id)?;
    if let Some((active, bound_agent_hashes)) =
        crate::subagent_orchestrator::active_reservation_projection(
            state_root,
            runtime_id,
            session_id,
            current_timestamp_millis(),
        )?
    {
        return Ok(active + marker_hashes.difference(&bound_agent_hashes).count());
    }
    Ok(marker_hashes.len())
}

/// Returns the validated provider-identity hashes represented by legacy active
/// markers. Modern lifecycle decisions use the ledger, but comparing this set
/// with bound ledger identities prevents an untracked lifecycle event from
/// being mistaken for a verified read-only batch.
pub(super) fn active_marker_hashes_for_runtime(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
) -> Result<BTreeSet<String>> {
    let session_dir = session_state_dir(state_root, session_id);
    let entries = match fs::read_dir(&session_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeSet::new());
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("读取 Codex 子代理门禁状态失败：{}", session_dir.display())
            });
        }
    };
    let expected_runtime_id_hash = hash_component(runtime_id);
    let prefix = runtime_marker_prefix(runtime_id);
    let mut hashes = BTreeSet::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !marker_name_has_prefix(&entry.path(), &prefix) {
            continue;
        }
        let path = entry.path();
        let bytes = crate::fs_util::read_bounded(&path, 16 * 1024)
            .with_context(|| format!("读取 Codex 子代理门禁状态失败：{}", path.display()))?;
        let marker = serde_json::from_slice::<ActiveMarker>(&bytes)
            .with_context(|| format!("解析 Codex 子代理门禁状态失败：{}", path.display()))?;
        anyhow::ensure!(
            marker.schema_version == ACTIVE_MARKER_SCHEMA_VERSION,
            "Codex 子代理门禁状态版本不受支持：{}",
            path.display()
        );
        anyhow::ensure!(
            marker.runtime_id_hash == expected_runtime_id_hash,
            "Codex 子代理门禁状态代次不一致：{}",
            path.display()
        );
        let file_name = path
            .file_name()
            .and_then(OsStr::to_str)
            .context("Codex 子代理门禁 marker 文件名不是有效 UTF-8")?;
        let agent_id_hash = file_name
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(".active"))
            .context("Codex 子代理门禁 marker 文件名格式无效")?;
        anyhow::ensure!(
            agent_id_hash.len() == 64
                && agent_id_hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "Codex 子代理门禁 marker 身份哈希格式无效：{}",
            path.display()
        );
        hashes.insert(agent_id_hash.to_string());
    }
    Ok(hashes)
}

pub(super) fn session_state_dir(state_root: &Path, session_id: &str) -> PathBuf {
    state_root.join(hash_component(session_id))
}

pub(super) fn agent_marker_path(session_dir: &Path, runtime_id: &str, agent_id: &str) -> PathBuf {
    agent_marker_path_from_hash(session_dir, runtime_id, &hash_component(agent_id))
}

pub(super) fn agent_marker_path_from_hash(
    session_dir: &Path,
    runtime_id: &str,
    agent_id_hash: &str,
) -> PathBuf {
    session_dir.join(format!(
        "{}{agent_id_hash}.active",
        runtime_marker_prefix(runtime_id)
    ))
}

pub(super) fn session_auxiliary_path(
    session_dir: &Path,
    runtime_id: &str,
    file_name: &str,
) -> PathBuf {
    session_dir.join(format!("{}{file_name}", runtime_marker_prefix(runtime_id)))
}

pub(super) fn runtime_marker_prefix(runtime_id: &str) -> String {
    format!("{}-", hash_component(runtime_id))
}

pub(super) fn marker_name_has_prefix(path: &Path, prefix: &str) -> bool {
    path.extension().and_then(OsStr::to_str) == Some("active")
        && runtime_file_has_prefix(path, prefix)
}

pub(super) fn runtime_file_has_prefix(path: &Path, prefix: &str) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with(prefix))
}

pub(super) fn hash_component(value: &str) -> String {
    crate::fs_util::sha256_hex(value.as_bytes())
}

pub(super) fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json(&map[key]));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

fn error_is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
