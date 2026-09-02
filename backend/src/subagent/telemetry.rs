use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::api::{TokenUsage, TraceContext};

const TRACE_SCHEMA_VERSION: u32 = 1;
const TRACE_FILE: &str = "subagent-traces-v1.jsonl";
const TRACE_ARCHIVE_FILE: &str = "subagent-traces-v1.previous.jsonl";
const TRACE_ARCHIVE_BACKUP_FILE: &str = "subagent-traces-v1.previous.backup.jsonl";
const TRACE_LOCK_FILE: &str = "subagent-traces-v1.lock";
const MAX_TRACE_BYTES: u64 = 8 * 1024 * 1024;
const TRACE_LOCK_TIMEOUT_MILLIS: u64 = 20;
const TRACE_LOCK_RETRY_MILLIS: u64 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraceEventKind {
    Scheduled,
    Started,
    HookEvaluated,
    RuleEvaluated,
    Completed,
    Failed,
    Recovered,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Recovered,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentTraceEvent {
    pub schema_version: u32,
    pub timestamp_ms: u64,
    pub trace_id: String,
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub event: TraceEventKind,
    pub status: ExecutionStatus,
    pub runtime_id_hash: String,
    pub session_id_hash: String,
    pub task_id_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, Value>,
}

impl SubagentTraceEvent {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        now_ms: u64,
        trace: &TraceContext,
        event: TraceEventKind,
        status: ExecutionStatus,
        runtime_id: &str,
        session_id: &str,
        task_id: &str,
        agent_id: Option<&str>,
        role: Option<&str>,
    ) -> Self {
        Self {
            schema_version: TRACE_SCHEMA_VERSION,
            timestamp_ms: now_ms,
            trace_id: trace.trace_id.clone(),
            span_id: format!("{:016x}", Uuid::new_v4().as_u128() as u64),
            parent_id: trace.parent_id.clone(),
            event,
            status,
            runtime_id_hash: hash_identifier(runtime_id),
            session_id_hash: hash_identifier(session_id),
            task_id_hash: hash_identifier(task_id),
            agent_id_hash: agent_id.map(hash_identifier),
            role: role.map(ToString::to_string),
            latency_ms: None,
            usage: None,
            error_code: None,
            error_message: None,
            attributes: BTreeMap::new(),
        }
    }
}

pub(crate) struct TraceRecorder<'a> {
    state_root: &'a Path,
}

impl<'a> TraceRecorder<'a> {
    pub(crate) fn new(state_root: &'a Path) -> Self {
        Self { state_root }
    }

    pub(crate) fn record(&self, event: &SubagentTraceEvent) -> Result<()> {
        self.record_with_lock_timeout(event, Duration::from_millis(TRACE_LOCK_TIMEOUT_MILLIS))
    }

    fn record_with_lock_timeout(
        &self,
        event: &SubagentTraceEvent,
        lock_timeout: Duration,
    ) -> Result<()> {
        let mut encoded = serde_json::to_vec(event).context("序列化子代理 trace 失败")?;
        encoded.push(b'\n');
        fs::create_dir_all(self.state_root)
            .with_context(|| format!("创建子代理观测目录失败：{}", self.state_root.display()))?;
        let lock_path = self.state_root.join(TRACE_LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&lock_path)
            .with_context(|| format!("打开子代理 trace 锁失败：{}", lock_path.display()))?;
        acquire_trace_lock(&lock, &lock_path, lock_timeout)?;
        let write_result = (|| -> Result<()> {
            let path = trace_file(self.state_root);
            rotate_if_needed(self.state_root, &path, encoded.len() as u64)?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("打开子代理 trace 文件失败：{}", path.display()))?;
            file.write_all(&encoded).context("追加子代理 trace 失败")?;
            file.flush().context("刷新子代理 trace 失败")?;
            Ok(())
        })();
        let _ = FileExt::unlock(&lock);
        write_result
    }

    pub(crate) fn record_best_effort(&self, event: &SubagentTraceEvent) {
        if let Err(error) = self.record(event) {
            eprintln!("Codey 子代理 trace 写入失败：{error:#}");
        }
    }
}

fn acquire_trace_lock(lock: &std::fs::File, lock_path: &Path, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if trace_lock_is_contended(&error) => {
                if started.elapsed() >= timeout {
                    anyhow::bail!(
                        "获取子代理 trace 锁超时（{} ms，遥测已丢弃）：{}",
                        timeout.as_millis(),
                        lock_path.display()
                    );
                }
                thread::sleep(Duration::from_millis(TRACE_LOCK_RETRY_MILLIS));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("锁定子代理 trace 失败：{}", lock_path.display()));
            }
        }
    }
}

fn trace_lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || (cfg!(windows) && error.raw_os_error() == Some(33))
}

fn rotate_if_needed(state_root: &Path, path: &Path, incoming_bytes: u64) -> Result<()> {
    rotate_if_needed_with_limit(state_root, path, incoming_bytes, MAX_TRACE_BYTES)
}

fn rotate_if_needed_with_limit(
    state_root: &Path,
    path: &Path,
    incoming_bytes: u64,
    max_bytes: u64,
) -> Result<()> {
    let current_bytes = fs::metadata(path).map_or(0, |metadata| metadata.len());
    if current_bytes == 0 || current_bytes.saturating_add(incoming_bytes) <= max_bytes {
        return Ok(());
    }
    let archive = state_root.join(TRACE_ARCHIVE_FILE);
    replace_trace_archive(state_root, path, &archive)
}

fn replace_trace_archive(state_root: &Path, path: &Path, archive: &Path) -> Result<()> {
    let backup = state_root.join(TRACE_ARCHIVE_BACKUP_FILE);
    remove_if_present(&backup, "清理子代理 trace 临时归档失败")?;
    let had_archive = archive.exists();
    if had_archive {
        fs::rename(archive, &backup).with_context(|| {
            format!(
                "暂存旧子代理 trace 归档失败：{} -> {}",
                archive.display(),
                backup.display()
            )
        })?;
    }
    if let Err(error) = fs::rename(path, archive) {
        if had_archive {
            let _ = fs::rename(&backup, archive);
        }
        return Err(error).with_context(|| {
            format!(
                "轮换子代理 trace 失败：{} -> {}",
                path.display(),
                archive.display()
            )
        });
    }
    let _ = remove_if_present(&backup, "清理子代理 trace 归档备份失败");
    Ok(())
}

fn remove_if_present(path: &Path, context: &str) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("{context}：{}", path.display())),
    }
}

pub(crate) fn trace_file(state_root: &Path) -> PathBuf {
    state_root.join(TRACE_FILE)
}

pub(crate) fn extract_token_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let value = value?;
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::Object(values) => {
                let input = unsigned_field(
                    values,
                    &[
                        "input_tokens",
                        "inputTokens",
                        "prompt_tokens",
                        "promptTokens",
                    ],
                );
                let output = unsigned_field(
                    values,
                    &[
                        "output_tokens",
                        "outputTokens",
                        "completion_tokens",
                        "completionTokens",
                    ],
                );
                let cached = unsigned_field(values, &["cached_tokens", "cachedTokens"]);
                let reasoning = unsigned_field(values, &["reasoning_tokens", "reasoningTokens"]);
                if input.is_some() || output.is_some() || cached.is_some() || reasoning.is_some() {
                    return Some(TokenUsage {
                        input_tokens: input.unwrap_or(0),
                        output_tokens: output.unwrap_or(0),
                        cached_tokens: cached.unwrap_or(0),
                        reasoning_tokens: reasoning.unwrap_or(0),
                    });
                }
                stack.extend(values.values());
            }
            Value::Array(values) => stack.extend(values),
            _ => {}
        }
    }
    None
}

fn unsigned_field(values: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| values.get(*key).and_then(Value::as_u64))
}

fn hash_identifier(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn token_usage_accepts_common_provider_shapes_without_payload_capture() {
        let usage = extract_token_usage(Some(&json!({
            "result": {
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 5,
                    "cached_tokens": 3,
                    "reasoning_tokens": 2
                }
            }
        })))
        .unwrap();
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.input_tokens + usage.output_tokens, 17);
    }

    #[test]
    fn trace_records_only_hashed_runtime_session_task_and_agent_ids() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceContext::new(None);
        let event = SubagentTraceEvent::new(
            10,
            &trace,
            TraceEventKind::Started,
            ExecutionStatus::Running,
            "runtime-secret",
            "session-secret",
            "task-secret",
            Some("agent-secret"),
            Some("codey_worker"),
        );
        TraceRecorder::new(temp.path()).record(&event).unwrap();
        let contents = fs::read_to_string(trace_file(temp.path())).unwrap();
        for secret in [
            "runtime-secret",
            "session-secret",
            "task-secret",
            "agent-secret",
        ] {
            assert!(!contents.contains(secret));
        }
        let decoded: SubagentTraceEvent = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(decoded.trace_id, trace.trace_id);
        assert_eq!(decoded.status, ExecutionStatus::Running);
    }

    #[test]
    fn concurrent_trace_writers_emit_complete_json_lines() {
        let temp = tempfile::tempdir().unwrap();
        let mut handles = Vec::new();
        for index in 0..8 {
            let root = temp.path().to_path_buf();
            handles.push(std::thread::spawn(move || {
                let trace = TraceContext::new(None);
                let event = SubagentTraceEvent::new(
                    index,
                    &trace,
                    TraceEventKind::Started,
                    ExecutionStatus::Running,
                    "runtime",
                    "session",
                    &format!("task-{index}"),
                    None,
                    Some("codey_worker"),
                );
                TraceRecorder::new(&root)
                    .record_with_lock_timeout(&event, Duration::from_secs(5))
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let contents = fs::read_to_string(trace_file(temp.path())).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 8);
        assert!(
            lines
                .iter()
                .all(|line| serde_json::from_str::<SubagentTraceEvent>(line).is_ok())
        );
    }

    #[test]
    fn trace_lock_contention_is_bounded_and_does_not_poison_later_writes() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        let lock_path = temp.path().join(TRACE_LOCK_FILE);
        let held_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        held_lock.lock_exclusive().unwrap();

        let trace = TraceContext::new(None);
        let event = SubagentTraceEvent::new(
            10,
            &trace,
            TraceEventKind::Started,
            ExecutionStatus::Running,
            "runtime",
            "session",
            "task",
            None,
            Some("codey_worker"),
        );
        let started = Instant::now();
        let error = TraceRecorder::new(temp.path()).record(&event).unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(format!("{error:#}").contains("遥测已丢弃"));
        assert!(!trace_file(temp.path()).exists());

        FileExt::unlock(&held_lock).unwrap();
        TraceRecorder::new(temp.path()).record(&event).unwrap();
        assert_eq!(
            fs::read_to_string(trace_file(temp.path()))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn rotation_accounts_for_the_incoming_event_before_append() {
        let temp = tempfile::tempdir().unwrap();
        let current = trace_file(temp.path());
        let archive = temp.path().join(TRACE_ARCHIVE_FILE);
        fs::write(&current, b"12345\n").unwrap();
        fs::write(&archive, b"old\n").unwrap();

        rotate_if_needed_with_limit(temp.path(), &current, 5, 10).unwrap();

        assert!(!current.exists());
        assert_eq!(fs::read(&archive).unwrap(), b"12345\n");
        assert!(!temp.path().join(TRACE_ARCHIVE_BACKUP_FILE).exists());
    }
}
