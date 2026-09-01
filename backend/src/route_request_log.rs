use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use arc_swap::ArcSwapOption;
use rusqlite::{Connection, OpenFlags, params, params_from_iter, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::oneshot;

use crate::config::{RouteRequestLogBackend, RouteRequestLogConfig};

const SCHEMA_VERSION: u8 = 4;
const MAX_LOG_STRING_BYTES: usize = 512;
const MAX_CONSECUTIVE_WRITE_FAILURES: u32 = 3;
const PARTS_PER_MILLION: u64 = 1_000_000;
const NDJSON_FILE_NAME: &str = "route-requests.ndjson";
const SQLITE_FILE_NAME: &str = "route-requests.sqlite3";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const SQLITE_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const DEFAULT_QUERY_PAGE_SIZE: u64 = 25;
const MAX_QUERY_PAGE: u64 = 1_000_000;
const MAX_QUERY_PAGE_SIZE: u64 = 100;
const MAX_QUERY_SEARCH_BYTES: usize = 256;
const MAX_QUERY_FILTER_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RequestProtocol {
    Http,
    Sse,
    #[serde(rename = "ws")]
    WebSocket,
}

impl RequestProtocol {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Sse => "sse",
            Self::WebSocket => "ws",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpstreamTransport {
    Http,
    HttpSse,
    #[serde(rename = "ws")]
    WebSocket,
}

impl UpstreamTransport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::HttpSse => "http_sse",
            Self::WebSocket => "ws",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FirstByteSource {
    UpstreamHttpBody,
    #[serde(rename = "upstream_ws_event")]
    UpstreamWebSocketEvent,
}

impl FirstByteSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamHttpBody => "upstream_http_body",
            Self::UpstreamWebSocketEvent => "upstream_ws_event",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RequestStatus {
    Succeeded,
    Failed,
    Incomplete,
    Cancelled,
}

impl RequestStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Incomplete => "incomplete",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestTokenUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

impl RequestTokenUsage {
    fn reported(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.cache_creation_input_tokens.is_some()
            || self.reasoning_output_tokens.is_some()
            || self.total_tokens.is_some()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogEntry {
    pub schema_version: u8,
    pub request_id: String,
    pub trace_id: String,
    pub timestamp_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    pub requested_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_header_ms: Option<u64>,
    pub total_duration_ms: u64,
    pub queue_delay_ms: u64,
    pub token_usage: RequestTokenUsage,
    pub usage_reported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_unavailable_reason: Option<String>,
    pub request_protocol: RequestProtocol,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_transport: Option<UpstreamTransport>,
    pub request_kind: String,
    pub status: RequestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_error_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_reason: Option<String>,
    pub retry_count: u32,
    pub fallback_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_authority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_bridge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_byte_source: Option<FirstByteSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_fingerprint: Option<String>,
    pub subagent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_session_id: Option<String>,
    pub codex_session_is_parent: bool,
}

pub(crate) struct RouteRequestLogStart<'a> {
    pub request_id: &'a str,
    pub started_at: Instant,
    pub request_protocol: RequestProtocol,
    pub request_kind: &'a str,
    pub requested_model: &'a str,
    pub reasoning_effort: Option<&'a str>,
    pub thinking_budget_tokens: Option<u64>,
    pub codex_session_id: Option<&'a str>,
    pub codex_session_is_parent: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(crate) struct RouteRequestLogQuery {
    pub page: u64,
    pub page_size: u64,
    pub search: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub protocol: Option<String>,
}

impl Default for RouteRequestLogQuery {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: DEFAULT_QUERY_PAGE_SIZE,
            search: None,
            provider: None,
            model: None,
            status: None,
            protocol: None,
        }
    }
}

impl RouteRequestLogQuery {
    fn normalize(mut self) -> anyhow::Result<Self> {
        if self.page == 0 || self.page > MAX_QUERY_PAGE {
            anyhow::bail!("页码必须在 1 到 {MAX_QUERY_PAGE} 之间");
        }
        if self.page_size == 0 || self.page_size > MAX_QUERY_PAGE_SIZE {
            anyhow::bail!("每页条数必须在 1 到 {MAX_QUERY_PAGE_SIZE} 之间");
        }
        normalize_query_value(&mut self.search, MAX_QUERY_SEARCH_BYTES, "搜索内容")?;
        normalize_query_value(&mut self.provider, MAX_QUERY_FILTER_BYTES, "供应商筛选")?;
        normalize_query_value(&mut self.model, MAX_QUERY_FILTER_BYTES, "模型筛选")?;
        normalize_query_value(&mut self.status, MAX_QUERY_FILTER_BYTES, "状态筛选")?;
        normalize_query_value(&mut self.protocol, MAX_QUERY_FILTER_BYTES, "协议筛选")?;
        self.status = self.status.map(|status| status.to_ascii_lowercase());
        self.protocol = self.protocol.map(|protocol| protocol.to_ascii_lowercase());
        if self.status.as_deref().is_some_and(|status| {
            !matches!(status, "succeeded" | "failed" | "incomplete" | "cancelled")
        }) {
            anyhow::bail!("状态筛选无效");
        }
        if self
            .protocol
            .as_deref()
            .is_some_and(|protocol| !matches!(protocol, "http" | "sse" | "ws"))
        {
            anyhow::bail!("协议筛选无效");
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogQueryPage {
    pub status: &'static str,
    pub backend: &'static str,
    pub queryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
    pub total_pages: u64,
    pub items: Vec<RouteRequestLogQueryItem>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogQueryItem {
    pub request_id: String,
    pub trace_id: String,
    pub timestamp_unix_ms: u64,
    pub provider: Option<String>,
    pub provider_name: Option<String>,
    pub requested_model: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub thinking_budget_tokens: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub upstream_header_ms: Option<u64>,
    pub total_duration_ms: u64,
    pub queue_delay_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub usage_reported: bool,
    pub usage_unavailable_reason: Option<String>,
    pub request_protocol: String,
    pub upstream_transport: Option<String>,
    pub request_kind: String,
    pub status: String,
    pub status_code: Option<u16>,
    pub upstream_status_code: Option<u16>,
    pub error_code: Option<String>,
    pub upstream_error_summary: Option<String>,
    pub completion_reason: Option<String>,
    pub retry_count: u32,
    pub fallback_count: u32,
    pub fallback_reason: Option<String>,
    pub upstream_authority: Option<String>,
    pub upstream_request_id: Option<String>,
    pub upstream_protocol: Option<String>,
    pub protocol_bridge: Option<String>,
    pub first_byte_source: Option<String>,
    pub subagent: bool,
    pub codex_session_id: Option<String>,
    pub codex_session_is_parent: bool,
}

#[derive(Debug, Default)]
struct RouteRequestLogStats {
    accepted: AtomicU64,
    sampled_out: AtomicU64,
    dropped_full: AtomicU64,
    dropped_closed: AtomicU64,
    write_failures: AtomicU64,
    write_dropped: AtomicU64,
    entries_written: AtomicU64,
    observer_panics: AtomicU64,
    writer_panics: AtomicU64,
    shutdown_timeouts: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogStatsSnapshot {
    pub accepted: u64,
    pub sampled_out: u64,
    pub dropped_full: u64,
    pub dropped_closed: u64,
    pub write_failures: u64,
    pub write_dropped: u64,
    pub entries_written: u64,
    pub observer_panics: u64,
    pub writer_panics: u64,
    pub shutdown_timeouts: u64,
}

impl RouteRequestLogStatsSnapshot {
    pub(crate) fn degraded(self) -> bool {
        self.dropped_full > 0
            || self.dropped_closed > 0
            || self.write_failures > 0
            || self.write_dropped > 0
            || self.observer_panics > 0
            || self.writer_panics > 0
            || self.shutdown_timeouts > 0
    }
}

impl RouteRequestLogStats {
    fn snapshot(&self) -> RouteRequestLogStatsSnapshot {
        RouteRequestLogStatsSnapshot {
            accepted: self.accepted.load(Ordering::Relaxed),
            sampled_out: self.sampled_out.load(Ordering::Relaxed),
            dropped_full: self.dropped_full.load(Ordering::Relaxed),
            dropped_closed: self.dropped_closed.load(Ordering::Relaxed),
            write_failures: self.write_failures.load(Ordering::Relaxed),
            write_dropped: self.write_dropped.load(Ordering::Relaxed),
            entries_written: self.entries_written.load(Ordering::Relaxed),
            observer_panics: self.observer_panics.load(Ordering::Relaxed),
            writer_panics: self.writer_panics.load(Ordering::Relaxed),
            shutdown_timeouts: self.shutdown_timeouts.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RouteRequestLogProducer {
    sender: SyncSender<QueuedEntry>,
    accepting: Arc<AtomicBool>,
    submitting: Arc<AtomicU64>,
    sample_rate_per_million: u32,
    sample_sequence: Arc<AtomicU64>,
    stats: Arc<RouteRequestLogStats>,
}

impl RouteRequestLogProducer {
    pub(crate) fn begin(&self, start: RouteRequestLogStart<'_>) -> Option<RouteRequestLogProbe> {
        self.catch_observer_panic(|| self.begin_inner(start))
            .flatten()
    }

    fn begin_inner(&self, start: RouteRequestLogStart<'_>) -> Option<RouteRequestLogProbe> {
        if !self.accepting.load(Ordering::Relaxed) {
            self.stats.dropped_closed.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if !self.should_sample() {
            self.stats.sampled_out.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let request_id = bounded_string(start.request_id);
        let entry = PendingEntry {
            request_id: request_id.clone(),
            trace_id: request_id,
            timestamp_unix_ms: unix_timestamp_ms_at(start.started_at),
            provider: None,
            provider_name: None,
            requested_model: bounded_string(start.requested_model),
            model: None,
            reasoning_effort: start.reasoning_effort.map(bounded_string),
            thinking_budget_tokens: start.thinking_budget_tokens,
            token_usage: RequestTokenUsage::default(),
            usage_unavailable_reason: None,
            request_protocol: start.request_protocol,
            upstream_transport: None,
            request_kind: bounded_string(start.request_kind),
            status: None,
            status_code: None,
            upstream_status_code: None,
            error_code: None,
            upstream_error_summary: None,
            completion_reason: None,
            retry_count: 0,
            fallback_count: 0,
            fallback_reason: None,
            upstream_authority: None,
            upstream_request_id: None,
            upstream_protocol: None,
            protocol_bridge: None,
            first_byte_source: None,
            client_fingerprint: None,
            subagent: false,
            codex_session_id: start.codex_session_id.map(bounded_string),
            codex_session_is_parent: start.codex_session_is_parent,
        };
        Some(RouteRequestLogProbe {
            shared: Arc::new(ProbeShared {
                producer: self.clone(),
                started_at: start.started_at,
                upstream_started_at: OnceLock::new(),
                first_byte_micros: AtomicU64::new(0),
                upstream_header_micros: AtomicU64::new(0),
                finished: AtomicBool::new(false),
                finish_gate: Mutex::new(ProbeFinishGate::default()),
                entry: Mutex::new(entry),
            }),
        })
    }

    fn catch_observer_panic<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        match catch_unwind(AssertUnwindSafe(operation)) {
            Ok(value) => Some(value),
            Err(_) => {
                self.stats.observer_panics.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn should_sample(&self) -> bool {
        let rate = u64::from(self.sample_rate_per_million);
        if rate >= PARTS_PER_MILLION {
            return true;
        }
        if rate == 0 {
            return false;
        }
        let sequence = self.sample_sequence.fetch_add(1, Ordering::Relaxed);
        mix64(sequence) % PARTS_PER_MILLION < rate
    }

    fn submit(&self, entry: RouteRequestLogEntry) {
        if !self.accepting.load(Ordering::Acquire) {
            self.stats.dropped_closed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.submitting.fetch_add(1, Ordering::AcqRel);
        if !self.accepting.load(Ordering::Acquire) {
            self.submitting.fetch_sub(1, Ordering::AcqRel);
            self.stats.dropped_closed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let queued = QueuedEntry {
            entry,
            enqueued_at: Instant::now(),
        };
        let result = self.sender.try_send(queued);
        self.submitting.fetch_sub(1, Ordering::AcqRel);
        match result {
            Ok(()) => {
                self.stats.accepted.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.stats.dropped_full.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.accepting.store(false, Ordering::Relaxed);
                self.stats.dropped_closed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct RouteRequestLogProbe {
    shared: Arc<ProbeShared>,
}

struct ProbeShared {
    producer: RouteRequestLogProducer,
    started_at: Instant,
    upstream_started_at: OnceLock<Instant>,
    first_byte_micros: AtomicU64,
    upstream_header_micros: AtomicU64,
    finished: AtomicBool,
    finish_gate: Mutex<ProbeFinishGate>,
    entry: Mutex<PendingEntry>,
}

#[derive(Default)]
struct ProbeFinishGate {
    observers: usize,
    pending: Option<(RequestStatus, &'static str)>,
}

pub(crate) struct RouteRequestLogFinishGuard {
    probe: RouteRequestLogProbe,
    active: bool,
}

impl Drop for RouteRequestLogFinishGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        self.probe.release_finish_observer();
    }
}

struct PendingEntry {
    request_id: String,
    trace_id: String,
    timestamp_unix_ms: u64,
    provider: Option<String>,
    provider_name: Option<String>,
    requested_model: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    thinking_budget_tokens: Option<u64>,
    token_usage: RequestTokenUsage,
    usage_unavailable_reason: Option<String>,
    request_protocol: RequestProtocol,
    upstream_transport: Option<UpstreamTransport>,
    request_kind: String,
    status: Option<RequestStatus>,
    status_code: Option<u16>,
    upstream_status_code: Option<u16>,
    error_code: Option<String>,
    upstream_error_summary: Option<String>,
    completion_reason: Option<String>,
    retry_count: u32,
    fallback_count: u32,
    fallback_reason: Option<String>,
    upstream_authority: Option<String>,
    upstream_request_id: Option<String>,
    upstream_protocol: Option<String>,
    protocol_bridge: Option<String>,
    first_byte_source: Option<FirstByteSource>,
    client_fingerprint: Option<String>,
    subagent: bool,
    codex_session_id: Option<String>,
    codex_session_is_parent: bool,
}

impl RouteRequestLogProbe {
    /// Defers final submission while a best-effort response observer drains.
    /// The request path never waits for the observer; dropping the guard
    /// releases the deferred exactly-once finish.
    pub(crate) fn defer_finish(&self) -> Option<RouteRequestLogFinishGuard> {
        let mut gate = lock_unpoisoned(&self.shared.finish_gate);
        if self.shared.finished.load(Ordering::Acquire) {
            return None;
        }
        gate.observers = gate.observers.saturating_add(1);
        Some(RouteRequestLogFinishGuard {
            probe: self.clone(),
            active: true,
        })
    }

    fn release_finish_observer(&self) {
        let pending = {
            let mut gate = lock_unpoisoned(&self.shared.finish_gate);
            gate.observers = gate.observers.saturating_sub(1);
            (gate.observers == 0).then(|| gate.pending.take()).flatten()
        };
        if let Some((status, reason)) = pending {
            self.finish_inner(status, reason);
        }
    }

    #[cfg(test)]
    pub(crate) fn detached_test_probe() -> Self {
        let (sender, _receiver) = mpsc::sync_channel(1);
        RouteRequestLogProducer {
            sender,
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::new(RouteRequestLogStats::default()),
        }
        .begin(RouteRequestLogStart {
            request_id: "test-probe",
            started_at: Instant::now(),
            request_protocol: RequestProtocol::Http,
            request_kind: "responses",
            requested_model: "test-model",
            reasoning_effort: None,
            thinking_budget_tokens: None,
            codex_session_id: None,
            codex_session_is_parent: false,
        })
        .expect("test request log probe must be sampled")
    }

    #[cfg(test)]
    pub(crate) fn token_usage_for_test(&self) -> RequestTokenUsage {
        lock_unpoisoned(&self.shared.entry).token_usage.clone()
    }

    #[cfg(test)]
    pub(crate) fn projected_metadata_for_test(
        &self,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let entry = lock_unpoisoned(&self.shared.entry);
        (
            entry.status.map(|status| status.as_str().to_string()),
            entry.error_code.clone(),
            entry.usage_unavailable_reason.clone(),
        )
    }

    fn shield(&self, operation: impl FnOnce()) {
        let _ = self.shared.producer.catch_observer_panic(operation);
    }

    pub(crate) fn set_request_protocol(&self, protocol: RequestProtocol) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).request_protocol = protocol;
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_route(
        &self,
        provider: &str,
        provider_name: &str,
        requested_model: &str,
        model: &str,
        upstream_authority: &str,
        upstream_protocol: &str,
        protocol_bridge: &str,
        subagent: bool,
    ) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            entry.provider = Some(bounded_string(provider));
            entry.provider_name = Some(bounded_string(provider_name));
            entry.requested_model = bounded_string(requested_model);
            entry.model = Some(bounded_string(model));
            entry.upstream_authority = Some(bounded_string(upstream_authority));
            entry.upstream_protocol = Some(bounded_string(upstream_protocol));
            entry.protocol_bridge = Some(bounded_string(protocol_bridge));
            entry.subagent = subagent;
        });
    }

    pub(crate) fn mark_upstream_send(&self, transport: UpstreamTransport) {
        self.shield(|| {
            let _ = self.shared.upstream_started_at.set(Instant::now());
            lock_unpoisoned(&self.shared.entry).upstream_transport = Some(transport);
        });
    }

    /// Updates the effective transport after a pre-send retry without moving
    /// the original upstream start instant used by TTFT/header latency.
    pub(crate) fn set_upstream_transport(&self, transport: UpstreamTransport) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).upstream_transport = Some(transport);
        });
    }

    pub(crate) fn mark_upstream_headers(
        &self,
        status_code: u16,
        upstream_request_id: Option<&str>,
    ) {
        self.shield(|| {
            if let Some(started_at) = self.shared.upstream_started_at.get() {
                store_elapsed_once(&self.shared.upstream_header_micros, *started_at);
            }
            let mut entry = lock_unpoisoned(&self.shared.entry);
            entry.upstream_status_code = Some(status_code);
            entry.upstream_request_id = upstream_request_id.map(bounded_string);
            if status_code >= 400 {
                entry.status = Some(RequestStatus::Failed);
            }
        });
    }

    pub(crate) fn mark_first_upstream_data(&self, source: FirstByteSource) {
        self.shield(|| {
            if self.shared.first_byte_micros.load(Ordering::Relaxed) != 0 {
                return;
            }
            let Some(started_at) = self.shared.upstream_started_at.get() else {
                return;
            };
            let elapsed = elapsed_micros(*started_at).saturating_add(1);
            if self
                .shared
                .first_byte_micros
                .compare_exchange(0, elapsed, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                lock_unpoisoned(&self.shared.entry).first_byte_source = Some(source);
            }
        });
    }

    pub(crate) fn mark_retry(&self, reason: &str) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            entry.retry_count = entry.retry_count.saturating_add(1);
            entry.fallback_reason = Some(bounded_string(reason));
        });
    }

    pub(crate) fn mark_fallback(&self, reason: &str) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            entry.fallback_count = entry.fallback_count.saturating_add(1);
            entry.fallback_reason = Some(bounded_string(reason));
        });
    }

    pub(crate) fn mark_error(&self, status_code: u16, error_code: &str) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            entry.status = Some(RequestStatus::Failed);
            entry.status_code = Some(status_code);
            entry.error_code = Some(bounded_string(error_code));
            entry.completion_reason = Some("error".to_string());
        });
    }

    pub(crate) fn mark_upstream_error_summary(&self, summary: &str) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            if entry.upstream_error_summary.is_none() {
                let summary = bounded_string(summary);
                if !summary.is_empty() {
                    entry.upstream_error_summary = Some(summary);
                }
            }
        });
    }

    pub(crate) fn mark_response_started(&self, status_code: u16) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            if entry.status_code.is_none() {
                entry.status_code = Some(status_code);
            }
        });
    }

    pub(crate) fn mark_usage_unavailable(&self, reason: &str) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            if !entry.token_usage.reported() {
                entry.usage_unavailable_reason = Some(bounded_string(reason));
            }
        });
    }

    pub(crate) fn mark_cancelled(&self, error_code: &str) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            entry.status = Some(RequestStatus::Cancelled);
            entry.error_code = Some(bounded_string(error_code));
            entry.completion_reason = Some("downstream_cancelled".to_string());
        });
    }

    pub(crate) fn observe_response(&self, status_code: u16, value: &Value) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            entry.status_code = Some(status_code);
            merge_usage(&mut entry.token_usage, value);
            if entry.token_usage.reported() {
                entry.usage_unavailable_reason = None;
            }
            observe_terminal_value(&mut entry, value);
            if entry.status.is_none() {
                entry.status = Some(if status_code < 400 {
                    RequestStatus::Succeeded
                } else {
                    RequestStatus::Failed
                });
            }
        });
    }

    pub(crate) fn observe_event(&self, event: &Value) {
        self.shield(|| {
            let event_type = event.get("type").and_then(Value::as_str);
            let contains_usage = usage_value(event).is_some();
            if !contains_usage
                && !matches!(
                    event_type,
                    Some(
                        "response.completed" | "response.failed" | "response.incomplete" | "error"
                    )
                )
            {
                return;
            }
            let mut entry = lock_unpoisoned(&self.shared.entry);
            merge_usage(&mut entry.token_usage, event);
            if entry.token_usage.reported() {
                entry.usage_unavailable_reason = None;
            }
            observe_terminal_value(&mut entry, event);
        });
    }

    /// Applies the small terminal metadata projection produced by the raw
    /// HTTP/SSE observer without materializing the full upstream response.
    pub(crate) fn observe_terminal_projection(
        &self,
        event_type: Option<&str>,
        response_status: Option<&str>,
        error_code: Option<&str>,
    ) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            apply_terminal_status(&mut entry, event_type.or(response_status));
            if entry.error_code.is_none() {
                entry.error_code = error_code
                    .map(bounded_string)
                    .filter(|error_code| !error_code.is_empty());
            }
        });
    }

    pub(crate) fn finish_success(&self) {
        self.finish(RequestStatus::Succeeded, "response_complete");
    }

    pub(crate) fn finish_cancelled(&self) {
        self.finish(RequestStatus::Cancelled, "scope_dropped");
    }

    fn finish(&self, default_status: RequestStatus, default_reason: &'static str) {
        self.shield(|| self.finish_inner(default_status, default_reason));
    }

    fn finish_inner(&self, default_status: RequestStatus, default_reason: &'static str) {
        {
            let mut gate = lock_unpoisoned(&self.shared.finish_gate);
            if gate.observers != 0 {
                gate.pending.get_or_insert((default_status, default_reason));
                return;
            }
            if self.shared.finished.swap(true, Ordering::AcqRel) {
                return;
            }
        }
        let mut pending = lock_unpoisoned(&self.shared.entry);
        let status = pending.status.unwrap_or(default_status);
        if pending.completion_reason.is_none() {
            pending.completion_reason = Some(default_reason.to_string());
        }
        let token_usage = std::mem::take(&mut pending.token_usage);
        let usage_reported = token_usage.reported();
        let usage_unavailable_reason = if usage_reported {
            None
        } else {
            pending.usage_unavailable_reason.take().or_else(|| {
                Some(
                    match status {
                        RequestStatus::Succeeded | RequestStatus::Incomplete => {
                            "not_reported_by_upstream"
                        }
                        RequestStatus::Failed | RequestStatus::Cancelled => "request_not_completed",
                    }
                    .to_string(),
                )
            })
        };
        let status_code = pending.status_code.or_else(|| {
            matches!(status, RequestStatus::Succeeded | RequestStatus::Incomplete).then_some(200)
        });
        let entry = RouteRequestLogEntry {
            schema_version: SCHEMA_VERSION,
            request_id: std::mem::take(&mut pending.request_id),
            trace_id: std::mem::take(&mut pending.trace_id),
            timestamp_unix_ms: pending.timestamp_unix_ms,
            provider: pending.provider.take(),
            provider_name: pending.provider_name.take(),
            requested_model: std::mem::take(&mut pending.requested_model),
            model: pending.model.take(),
            reasoning_effort: pending.reasoning_effort.take(),
            thinking_budget_tokens: pending.thinking_budget_tokens,
            ttft_ms: load_duration_ms(&self.shared.first_byte_micros),
            upstream_header_ms: load_duration_ms(&self.shared.upstream_header_micros),
            total_duration_ms: elapsed_millis(self.shared.started_at),
            queue_delay_ms: 0,
            usage_reported,
            usage_unavailable_reason,
            token_usage,
            request_protocol: pending.request_protocol,
            upstream_transport: pending.upstream_transport,
            request_kind: std::mem::take(&mut pending.request_kind),
            status,
            status_code,
            upstream_status_code: pending.upstream_status_code,
            error_code: pending.error_code.take(),
            upstream_error_summary: pending.upstream_error_summary.take(),
            completion_reason: pending.completion_reason.take(),
            retry_count: pending.retry_count,
            fallback_count: pending.fallback_count,
            fallback_reason: pending.fallback_reason.take(),
            upstream_authority: pending.upstream_authority.take(),
            upstream_request_id: pending.upstream_request_id.take(),
            upstream_protocol: pending.upstream_protocol.take(),
            protocol_bridge: pending.protocol_bridge.take(),
            first_byte_source: pending.first_byte_source,
            client_fingerprint: pending.client_fingerprint.take(),
            subagent: pending.subagent,
            codex_session_id: pending.codex_session_id.take(),
            codex_session_is_parent: pending.codex_session_is_parent,
        };
        drop(pending);
        self.shared.producer.submit(entry);
    }
}

pub(crate) struct RouteRequestLogGuard {
    probe: Option<RouteRequestLogProbe>,
}

impl RouteRequestLogGuard {
    pub(crate) fn new(probe: Option<RouteRequestLogProbe>) -> Self {
        Self { probe }
    }
}

impl Drop for RouteRequestLogGuard {
    fn drop(&mut self) {
        if let Some(probe) = self.probe.take() {
            probe.finish_cancelled();
        }
    }
}

pub(crate) struct RouteRequestLogController {
    active: ArcSwapOption<RouteRequestLogProducer>,
    state: AsyncMutex<RouteRequestLogControllerState>,
    root: PathBuf,
}

#[derive(Default)]
struct RouteRequestLogControllerState {
    config: Option<RouteRequestLogConfig>,
    runtime: Option<RouteRequestLogRuntime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteRequestLogReconfigure {
    Unchanged,
    Enabled,
    Disabled,
}

impl RouteRequestLogController {
    pub(crate) fn new() -> Self {
        Self::with_root(codey_runtime_core::paths::default_app_state_dir())
    }

    fn with_root(root: PathBuf) -> Self {
        Self {
            active: ArcSwapOption::empty(),
            state: AsyncMutex::new(RouteRequestLogControllerState::default()),
            root,
        }
    }

    /// The disabled fast path is a single atomic load. The start closure is
    /// deliberately lazy so extracting optional observation fields is skipped
    /// entirely while logging is off.
    pub(crate) fn begin(
        &self,
        begin: impl FnOnce(&RouteRequestLogProducer) -> Option<RouteRequestLogProbe>,
    ) -> Option<RouteRequestLogProbe> {
        let producer = self.active.load_full()?;
        begin(&producer)
    }

    pub(crate) async fn reconfigure(
        &self,
        config: &RouteRequestLogConfig,
    ) -> anyhow::Result<RouteRequestLogReconfigure> {
        let mut state = self.state.lock().await;
        let desired_active = config.enabled && config.sample_rate_per_million > 0;
        let currently_healthy = self
            .active
            .load_full()
            .is_some_and(|producer| producer.accepting.load(Ordering::Acquire));
        if state.config.as_ref() == Some(config)
            && desired_active == currently_healthy
            && (desired_active || state.runtime.is_none())
        {
            return Ok(RouteRequestLogReconfigure::Unchanged);
        }

        if !desired_active {
            self.active.store(None);
            let retiring = state.runtime.take();
            state.config = Some(config.clone());
            if let Some(retiring) = retiring {
                retiring.stop().await;
            }
            return Ok(RouteRequestLogReconfigure::Disabled);
        }

        // Never leave two generations writing the same sink. SQLite can
        // technically serialize two writers, but NDJSON rotation cannot, and
        // a short best-effort observation gap is safer than file corruption.
        self.active.store(None);
        if let Some(retiring) = state.runtime.take() {
            retiring.stop().await;
        }
        state.config = Some(config.clone());

        let worker_config = config.clone();
        let root = self.root.clone();
        let started = tokio::task::spawn_blocking(move || {
            RouteRequestLogRuntime::start_at(&worker_config, root)
        })
        .await
        .map_err(|error| anyhow::anyhow!("请求日志 writer 启动任务异常退出：{error}"))??
        .ok_or_else(|| anyhow::anyhow!("请求日志配置未启用 writer"))?;
        let (producer, runtime) = started;

        // Publish only after the worker has opened and initialized its sink.
        self.active.store(Some(Arc::new(producer)));
        state.runtime = Some(runtime);
        Ok(RouteRequestLogReconfigure::Enabled)
    }

    pub(crate) async fn stop(&self) -> Option<RouteRequestLogStatsSnapshot> {
        let mut state = self.state.lock().await;
        self.active.store(None);
        if let Some(runtime) = state.runtime.take() {
            runtime.stop().await;
            return Some(runtime.stats_snapshot());
        }
        None
    }
}

impl Drop for RouteRequestLogController {
    fn drop(&mut self) {
        self.active.store(None);
        if let Some(runtime) = self.state.get_mut().runtime.take() {
            runtime.request_shutdown();
        }
    }
}

pub(crate) struct RouteRequestLogRuntime {
    done: Mutex<Option<oneshot::Receiver<()>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    shutdown: Mutex<Option<Sender<WriterControl>>>,
    accepting: Arc<AtomicBool>,
    shutdown_timeout: Duration,
    stats: Arc<RouteRequestLogStats>,
}

impl RouteRequestLogRuntime {
    fn start_at(
        config: &RouteRequestLogConfig,
        root: PathBuf,
    ) -> anyhow::Result<Option<(RouteRequestLogProducer, Self)>> {
        if !config.enabled || config.sample_rate_per_million == 0 {
            return Ok(None);
        }
        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
        let (control_tx, control_rx) = mpsc::channel();
        let stats = Arc::new(RouteRequestLogStats::default());
        let accepting = Arc::new(AtomicBool::new(true));
        let submitting = Arc::new(AtomicU64::new(0));
        let producer = RouteRequestLogProducer {
            sender,
            accepting: Arc::clone(&accepting),
            submitting: Arc::clone(&submitting),
            sample_rate_per_million: config.sample_rate_per_million,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::clone(&stats),
        };
        let worker_config = WorkerConfig::new(config, root);
        let worker_stats = Arc::clone(&stats);
        let worker_accepting = Arc::clone(&accepting);
        let worker_submitting = Arc::clone(&submitting);
        let (done_tx, done_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = match thread::Builder::new()
            .name("codey-route-request-log".to_string())
            .spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let sink = match BatchSink::open(&worker_config) {
                        Ok(sink) => {
                            let _ = ready_tx.send(Ok(()));
                            sink
                        }
                        Err(error) => {
                            worker_stats.write_failures.fetch_add(1, Ordering::Relaxed);
                            let _ = ready_tx.send(Err(error.to_string()));
                            return;
                        }
                    };
                    writer_loop(
                        receiver,
                        control_rx,
                        sink,
                        worker_config,
                        &worker_accepting,
                        &worker_submitting,
                        &worker_stats,
                    )
                }));
                if outcome.is_err() {
                    worker_stats.writer_panics.fetch_add(1, Ordering::Relaxed);
                }
                worker_accepting.store(false, Ordering::Release);
                let _ = done_tx.send(());
            }) {
            Ok(worker) => worker,
            Err(error) => {
                return Err(anyhow::Error::new(error).context("创建请求日志 writer 线程失败"));
            }
        };
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = worker.join();
                anyhow::bail!("打开请求日志存储失败：{error}");
            }
            Err(_) => {
                let _ = worker.join();
                anyhow::bail!("请求日志 writer 在就绪前异常退出");
            }
        }
        Ok(Some((
            producer,
            Self {
                done: Mutex::new(Some(done_rx)),
                worker: Mutex::new(Some(worker)),
                shutdown: Mutex::new(Some(control_tx)),
                accepting,
                shutdown_timeout: Duration::from_millis(config.shutdown_flush_timeout_ms),
                stats,
            },
        )))
    }

    fn request_shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        if let Some(shutdown) = lock_unpoisoned(&self.shutdown).take() {
            let _ = shutdown.send(WriterControl::Shutdown);
        }
    }

    pub(crate) async fn stop(&self) {
        self.request_shutdown();
        let done = lock_unpoisoned(&self.done).take();
        let completed = match done {
            Some(done) => tokio::time::timeout(self.shutdown_timeout, done)
                .await
                .is_ok(),
            None => true,
        };
        let worker = lock_unpoisoned(&self.worker).take();
        if completed {
            if let Some(worker) = worker {
                let _ = worker.join();
            }
        } else {
            self.stats.shutdown_timeouts.fetch_add(1, Ordering::Relaxed);
            drop(worker);
        }
    }

    pub(crate) fn stats_snapshot(&self) -> RouteRequestLogStatsSnapshot {
        self.stats.snapshot()
    }
}

impl Drop for RouteRequestLogRuntime {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        if let Some(shutdown) = self
            .shutdown
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = shutdown.send(WriterControl::Shutdown);
        }
    }
}

enum WriterControl {
    Shutdown,
}

struct QueuedEntry {
    entry: RouteRequestLogEntry,
    enqueued_at: Instant,
}

#[derive(Clone)]
struct WorkerConfig {
    backend: RouteRequestLogBackend,
    batch_size: usize,
    flush_interval: Duration,
    max_file_bytes: u64,
    retained_files: usize,
    retention_days: u32,
    root: PathBuf,
}

impl WorkerConfig {
    fn new(config: &RouteRequestLogConfig, root: PathBuf) -> Self {
        Self {
            backend: config.backend,
            batch_size: config.batch_size,
            flush_interval: Duration::from_millis(config.flush_interval_ms),
            max_file_bytes: config.max_file_bytes,
            retained_files: config.retained_files,
            retention_days: config.retention_days,
            root,
        }
    }
}

fn writer_loop(
    receiver: Receiver<QueuedEntry>,
    control: Receiver<WriterControl>,
    mut sink: BatchSink,
    config: WorkerConfig,
    accepting: &AtomicBool,
    submitting: &AtomicU64,
    stats: &RouteRequestLogStats,
) {
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut deadline = Instant::now() + config.flush_interval;
    let mut consecutive_failures = 0_u32;
    loop {
        if control.try_recv().is_ok() {
            drain_writer_for_shutdown(
                &receiver,
                &mut sink,
                &mut batch,
                submitting,
                stats,
                &mut consecutive_failures,
            );
            return;
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(50));
        match receiver.recv_timeout(wait) {
            Ok(entry) => {
                batch.push(entry);
                while batch.len() < config.batch_size {
                    match receiver.try_recv() {
                        Ok(entry) => batch.push(entry),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => break,
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if !batch.is_empty() {
                    let _ = flush_batch(&mut sink, &mut batch, stats, &mut consecutive_failures);
                }
                let _ = sink.finish();
                return;
            }
        }
        if batch.len() >= config.batch_size || Instant::now() >= deadline {
            if !batch.is_empty()
                && !flush_batch(&mut sink, &mut batch, stats, &mut consecutive_failures)
            {
                accepting.store(false, Ordering::Release);
                return;
            }
            deadline = Instant::now() + config.flush_interval;
        }
    }
}

fn drain_writer_for_shutdown(
    receiver: &Receiver<QueuedEntry>,
    sink: &mut BatchSink,
    batch: &mut Vec<QueuedEntry>,
    submitting: &AtomicU64,
    stats: &RouteRequestLogStats,
    consecutive_failures: &mut u32,
) {
    loop {
        while batch.len() < batch.capacity() {
            match receiver.try_recv() {
                Ok(entry) => batch.push(entry),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if !batch.is_empty() && !flush_batch(sink, batch, stats, consecutive_failures) {
            break;
        }
        if submitting.load(Ordering::Acquire) == 0 {
            match receiver.try_recv() {
                Ok(entry) => {
                    batch.push(entry);
                    continue;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        thread::yield_now();
    }
    let _ = sink.finish();
}

fn flush_batch(
    sink: &mut BatchSink,
    batch: &mut Vec<QueuedEntry>,
    stats: &RouteRequestLogStats,
    consecutive_failures: &mut u32,
) -> bool {
    let now = Instant::now();
    for queued in batch.iter_mut() {
        queued.entry.queue_delay_ms =
            duration_millis(now.saturating_duration_since(queued.enqueued_at));
    }
    match sink.write_batch(batch) {
        Ok(()) => {
            stats
                .entries_written
                .fetch_add(batch.len() as u64, Ordering::Relaxed);
            *consecutive_failures = 0;
        }
        Err(_) => {
            stats.write_failures.fetch_add(1, Ordering::Relaxed);
            stats
                .write_dropped
                .fetch_add(batch.len() as u64, Ordering::Relaxed);
            *consecutive_failures = consecutive_failures.saturating_add(1);
        }
    }
    batch.clear();
    *consecutive_failures < MAX_CONSECUTIVE_WRITE_FAILURES
}

enum BatchSink {
    Ndjson(NdjsonSink),
    Sqlite(SqliteSink),
}

impl BatchSink {
    fn open(config: &WorkerConfig) -> std::io::Result<Self> {
        fs::create_dir_all(&config.root)?;
        match config.backend {
            RouteRequestLogBackend::Ndjson => NdjsonSink::open(
                config.root.join(NDJSON_FILE_NAME),
                config.max_file_bytes,
                config.retained_files,
            )
            .map(Self::Ndjson),
            RouteRequestLogBackend::Sqlite => {
                SqliteSink::open(config.root.join(SQLITE_FILE_NAME), config.retention_days)
                    .map(Self::Sqlite)
                    .map_err(std::io::Error::other)
            }
        }
    }

    fn write_batch(&mut self, batch: &[QueuedEntry]) -> anyhow::Result<()> {
        match self {
            Self::Ndjson(sink) => sink.write_batch(batch),
            Self::Sqlite(sink) => sink.write_batch(batch),
        }
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Ndjson(sink) => sink.finish(),
            Self::Sqlite(sink) => sink.finish(),
        }
    }
}

struct NdjsonSink {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    written_bytes: u64,
    max_file_bytes: u64,
    retained_files: usize,
}

impl NdjsonSink {
    fn open(path: PathBuf, max_file_bytes: u64, retained_files: usize) -> std::io::Result<Self> {
        let file = open_private_append_file(&path)?;
        let written_bytes = file.metadata()?.len();
        Ok(Self {
            path,
            writer: Some(BufWriter::new(file)),
            written_bytes,
            max_file_bytes,
            retained_files,
        })
    }

    fn write_batch(&mut self, batch: &[QueuedEntry]) -> anyhow::Result<()> {
        let mut encoded = Vec::with_capacity(batch.len().saturating_mul(512));
        for queued in batch {
            serde_json::to_writer(&mut encoded, &queued.entry)?;
            encoded.push(b'\n');
        }
        if self.written_bytes > 0
            && self.written_bytes.saturating_add(encoded.len() as u64) > self.max_file_bytes
        {
            self.rotate()?;
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| std::io::Error::other("NDJSON writer is unavailable"))?;
        writer.write_all(&encoded)?;
        writer.flush()?;
        self.written_bytes = self.written_bytes.saturating_add(encoded.len() as u64);
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        // Windows does not allow an open file to be renamed. Take and drop the
        // writer before rotating so the same implementation is portable.
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
            drop(writer);
        }
        if self.retained_files > 0 {
            let oldest = rotated_path(&self.path, self.retained_files);
            remove_file_if_exists(&oldest)?;
            for index in (1..self.retained_files).rev() {
                let source = rotated_path(&self.path, index);
                let destination = rotated_path(&self.path, index + 1);
                rename_if_exists(&source, &destination)?;
            }
            rename_if_exists(&self.path, &rotated_path(&self.path, 1))?;
        } else {
            remove_file_if_exists(&self.path)?;
        }
        let file = open_private_append_file(&self.path)?;
        self.writer = Some(BufWriter::new(file));
        self.written_bytes = 0;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }
}

struct SqliteSink {
    connection: Connection,
    retention_ms: u64,
    next_prune_at: Instant,
}

impl SqliteSink {
    fn open(path: PathBuf, retention_days: u32) -> anyhow::Result<Self> {
        ensure_private_sqlite_file(&path)?;
        let connection = Connection::open(&path)?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS route_request_logs (
                request_id TEXT PRIMARY KEY,
                trace_id TEXT NOT NULL,
                timestamp_unix_ms INTEGER NOT NULL,
                provider TEXT,
                provider_name TEXT,
                requested_model TEXT NOT NULL,
                model TEXT,
                reasoning_effort TEXT,
                thinking_budget_tokens INTEGER,
                ttft_ms INTEGER,
                upstream_header_ms INTEGER,
                total_duration_ms INTEGER NOT NULL,
                queue_delay_ms INTEGER NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cached_input_tokens INTEGER,
                cache_creation_input_tokens INTEGER,
                reasoning_output_tokens INTEGER,
                total_tokens INTEGER,
                usage_reported INTEGER NOT NULL,
                usage_unavailable_reason TEXT,
                request_protocol TEXT NOT NULL,
                upstream_transport TEXT,
                request_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                status_code INTEGER,
                upstream_status_code INTEGER,
                error_code TEXT,
                upstream_error_summary TEXT,
                completion_reason TEXT,
                retry_count INTEGER NOT NULL,
                fallback_count INTEGER NOT NULL,
                fallback_reason TEXT,
                upstream_authority TEXT,
                upstream_request_id TEXT,
                upstream_protocol TEXT,
                protocol_bridge TEXT,
                first_byte_source TEXT,
                client_fingerprint TEXT,
                subagent INTEGER NOT NULL,
                schema_version INTEGER NOT NULL,
                codex_session_id TEXT,
                codex_session_is_parent INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_time
                ON route_request_logs(timestamp_unix_ms);
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_provider_model
                ON route_request_logs(provider, model, timestamp_unix_ms);
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_status
                ON route_request_logs(status, timestamp_unix_ms);",
        )?;
        ensure_sqlite_log_columns(&connection)?;
        let retention_ms = u64::from(retention_days)
            .saturating_mul(24 * 60 * 60)
            .saturating_mul(1_000);
        prune_sqlite_logs(&connection, retention_ms)?;
        Ok(Self {
            connection,
            retention_ms,
            next_prune_at: Instant::now() + SQLITE_PRUNE_INTERVAL,
        })
    }

    fn write_batch(&mut self, batch: &[QueuedEntry]) -> anyhow::Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO route_request_logs (
                    request_id, trace_id, timestamp_unix_ms, provider, provider_name,
                    requested_model, model, reasoning_effort, thinking_budget_tokens,
                    ttft_ms, upstream_header_ms, total_duration_ms, queue_delay_ms,
                    input_tokens, output_tokens, cached_input_tokens,
                    cache_creation_input_tokens, reasoning_output_tokens, total_tokens,
                    usage_reported, usage_unavailable_reason, request_protocol,
                    upstream_transport, request_kind,
                    status, status_code, upstream_status_code, error_code, completion_reason,
                    retry_count, fallback_count, fallback_reason, upstream_authority,
                    upstream_request_id, upstream_protocol, protocol_bridge,
                    first_byte_source, client_fingerprint, subagent, schema_version,
                    upstream_error_summary, codex_session_id, codex_session_is_parent
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                    ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                    ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                    ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40,
                    ?41, ?42, ?43
                )",
            )?;
            for queued in batch {
                let entry = &queued.entry;
                statement.execute(params![
                    entry.request_id,
                    entry.trace_id,
                    to_i64(entry.timestamp_unix_ms),
                    entry.provider,
                    entry.provider_name,
                    entry.requested_model,
                    entry.model,
                    entry.reasoning_effort,
                    entry.thinking_budget_tokens.map(to_i64),
                    entry.ttft_ms.map(to_i64),
                    entry.upstream_header_ms.map(to_i64),
                    to_i64(entry.total_duration_ms),
                    to_i64(entry.queue_delay_ms),
                    entry.token_usage.input_tokens.map(to_i64),
                    entry.token_usage.output_tokens.map(to_i64),
                    entry.token_usage.cached_input_tokens.map(to_i64),
                    entry.token_usage.cache_creation_input_tokens.map(to_i64),
                    entry.token_usage.reasoning_output_tokens.map(to_i64),
                    entry.token_usage.total_tokens.map(to_i64),
                    entry.usage_reported,
                    entry.usage_unavailable_reason,
                    entry.request_protocol.as_str(),
                    entry.upstream_transport.map(UpstreamTransport::as_str),
                    entry.request_kind,
                    entry.status.as_str(),
                    entry.status_code,
                    entry.upstream_status_code,
                    entry.error_code,
                    entry.completion_reason,
                    entry.retry_count,
                    entry.fallback_count,
                    entry.fallback_reason,
                    entry.upstream_authority,
                    entry.upstream_request_id,
                    entry.upstream_protocol,
                    entry.protocol_bridge,
                    entry.first_byte_source.map(FirstByteSource::as_str),
                    entry.client_fingerprint,
                    entry.subagent,
                    entry.schema_version,
                    entry.upstream_error_summary,
                    entry.codex_session_id,
                    entry.codex_session_is_parent,
                ])?;
            }
        }
        transaction.commit()?;
        if Instant::now() >= self.next_prune_at {
            // Retention is maintenance, not part of accepting the current
            // batch. A prune failure must not misreport committed rows as lost.
            let _ = prune_sqlite_logs(&self.connection, self.retention_ms);
            self.next_prune_at = Instant::now() + SQLITE_PRUNE_INTERVAL;
        }
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }
}

pub(crate) fn query_route_request_logs(
    root: &Path,
    backend: RouteRequestLogBackend,
    query: RouteRequestLogQuery,
) -> anyhow::Result<RouteRequestLogQueryPage> {
    let query = query.normalize()?;
    if backend == RouteRequestLogBackend::Ndjson {
        return Ok(RouteRequestLogQueryPage {
            status: "unavailable",
            backend: "ndjson",
            queryable: false,
            reason: Some("ndjson_not_queryable"),
            page: query.page,
            page_size: query.page_size,
            total: 0,
            total_pages: 0,
            items: Vec::new(),
        });
    }

    let path = root.join(SQLITE_FILE_NAME);
    if !path.is_file() {
        return Ok(empty_query_page(query.page, query.page_size));
    }
    query_sqlite_route_request_logs(&path, query)
}

fn query_sqlite_route_request_logs(
    path: &Path,
    query: RouteRequestLogQuery,
) -> anyhow::Result<RouteRequestLogQueryPage> {
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "query_only", "ON")?;
    let has_upstream_error_summary =
        sqlite_table_has_column(&connection, "upstream_error_summary")?;
    let has_codex_session_id = sqlite_table_has_column(&connection, "codex_session_id")?;
    let has_codex_session_is_parent =
        sqlite_table_has_column(&connection, "codex_session_is_parent")?;
    let transaction = connection.transaction()?;
    let (where_clause, filter_params) =
        sqlite_query_filters(&query, has_upstream_error_summary, has_codex_session_id);
    let count_sql = format!("SELECT COUNT(*) FROM route_request_logs{where_clause}");
    let total_i64: i64 =
        transaction.query_row(&count_sql, params_from_iter(filter_params.iter()), |row| {
            row.get(0)
        })?;
    let total = u64::try_from(total_i64).unwrap_or_default();
    let total_pages = total.div_ceil(query.page_size);
    let offset = query.page.saturating_sub(1).saturating_mul(query.page_size);
    let upstream_error_summary_column = if has_upstream_error_summary {
        "upstream_error_summary"
    } else {
        "NULL AS upstream_error_summary"
    };
    let codex_session_id_column = if has_codex_session_id {
        "codex_session_id"
    } else {
        "NULL AS codex_session_id"
    };
    let codex_session_is_parent_column = if has_codex_session_is_parent {
        "codex_session_is_parent"
    } else {
        "0 AS codex_session_is_parent"
    };
    let select_sql = format!(
        "SELECT
            request_id, trace_id, timestamp_unix_ms, provider, provider_name,
            requested_model, model, reasoning_effort, thinking_budget_tokens,
            ttft_ms, upstream_header_ms, total_duration_ms, queue_delay_ms,
            input_tokens, output_tokens, cached_input_tokens,
            cache_creation_input_tokens, reasoning_output_tokens, total_tokens,
            usage_reported, usage_unavailable_reason, request_protocol,
            upstream_transport, request_kind, status, status_code,
            upstream_status_code, error_code, completion_reason, retry_count,
            fallback_count, fallback_reason, upstream_authority,
            upstream_request_id, upstream_protocol, protocol_bridge,
            first_byte_source, subagent, {upstream_error_summary_column},
            {codex_session_id_column}, {codex_session_is_parent_column}
         FROM route_request_logs{where_clause}
         ORDER BY timestamp_unix_ms DESC, request_id DESC
         LIMIT ? OFFSET ?"
    );
    let mut page_params = filter_params;
    page_params.push(SqlValue::Integer(to_i64(query.page_size)));
    page_params.push(SqlValue::Integer(to_i64(offset)));
    let mut statement = transaction.prepare(&select_sql)?;
    let items = statement
        .query_map(params_from_iter(page_params), query_item_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    transaction.commit()?;

    Ok(RouteRequestLogQueryPage {
        status: "ok",
        backend: "sqlite",
        queryable: true,
        reason: None,
        page: query.page,
        page_size: query.page_size,
        total,
        total_pages,
        items,
    })
}

fn empty_query_page(page: u64, page_size: u64) -> RouteRequestLogQueryPage {
    RouteRequestLogQueryPage {
        status: "ok",
        backend: "sqlite",
        queryable: true,
        reason: None,
        page,
        page_size,
        total: 0,
        total_pages: 0,
        items: Vec::new(),
    }
}

fn sqlite_query_filters(
    query: &RouteRequestLogQuery,
    has_upstream_error_summary: bool,
    has_codex_session_id: bool,
) -> (String, Vec<SqlValue>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut values = Vec::new();
    if let Some(search) = &query.search {
        let pattern = format!("%{}%", escape_like_pattern(search));
        let mut search_clause = String::from(
            "(request_id LIKE ? ESCAPE '\\'
              OR trace_id LIKE ? ESCAPE '\\'
              OR COALESCE(provider, '') LIKE ? ESCAPE '\\'
              OR COALESCE(provider_name, '') LIKE ? ESCAPE '\\'
              OR requested_model LIKE ? ESCAPE '\\'
              OR COALESCE(model, '') LIKE ? ESCAPE '\\'
              OR COALESCE(error_code, '') LIKE ? ESCAPE '\\'
              OR COALESCE(upstream_request_id, '') LIKE ? ESCAPE '\\'",
        );
        values.extend((0..8).map(|_| SqlValue::Text(pattern.clone())));
        if has_upstream_error_summary {
            search_clause.push_str(" OR COALESCE(upstream_error_summary, '') LIKE ? ESCAPE '\\'");
            values.push(SqlValue::Text(pattern.clone()));
        }
        if has_codex_session_id {
            search_clause.push_str(" OR COALESCE(codex_session_id, '') LIKE ? ESCAPE '\\'");
            values.push(SqlValue::Text(pattern));
        }
        search_clause.push(')');
        clauses.push(search_clause);
    }
    if let Some(provider) = &query.provider {
        clauses.push("(provider = ? COLLATE NOCASE OR provider_name = ? COLLATE NOCASE)".into());
        values.push(SqlValue::Text(provider.clone()));
        values.push(SqlValue::Text(provider.clone()));
    }
    if let Some(model) = &query.model {
        clauses.push("(model = ? COLLATE NOCASE OR requested_model = ? COLLATE NOCASE)".into());
        values.push(SqlValue::Text(model.clone()));
        values.push(SqlValue::Text(model.clone()));
    }
    if let Some(status) = &query.status {
        clauses.push("status = ?".into());
        values.push(SqlValue::Text(status.clone()));
    }
    if let Some(protocol) = &query.protocol {
        clauses.push("request_protocol = ?".into());
        values.push(SqlValue::Text(protocol.clone()));
    }
    if clauses.is_empty() {
        (String::new(), values)
    } else {
        (format!(" WHERE {}", clauses.join(" AND ")), values)
    }
}

fn query_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RouteRequestLogQueryItem> {
    Ok(RouteRequestLogQueryItem {
        request_id: row.get(0)?,
        trace_id: row.get(1)?,
        timestamp_unix_ms: row_u64(row, 2)?,
        provider: row.get(3)?,
        provider_name: row.get(4)?,
        requested_model: row.get(5)?,
        model: row.get(6)?,
        reasoning_effort: row.get(7)?,
        thinking_budget_tokens: row_optional_u64(row, 8)?,
        ttft_ms: row_optional_u64(row, 9)?,
        upstream_header_ms: row_optional_u64(row, 10)?,
        total_duration_ms: row_u64(row, 11)?,
        queue_delay_ms: row_u64(row, 12)?,
        input_tokens: row_optional_u64(row, 13)?,
        output_tokens: row_optional_u64(row, 14)?,
        cached_input_tokens: row_optional_u64(row, 15)?,
        cache_creation_input_tokens: row_optional_u64(row, 16)?,
        reasoning_output_tokens: row_optional_u64(row, 17)?,
        total_tokens: row_optional_u64(row, 18)?,
        usage_reported: row.get(19)?,
        usage_unavailable_reason: row.get(20)?,
        request_protocol: row.get(21)?,
        upstream_transport: row.get(22)?,
        request_kind: row.get(23)?,
        status: row.get(24)?,
        status_code: row_optional_u16(row, 25)?,
        upstream_status_code: row_optional_u16(row, 26)?,
        error_code: row.get(27)?,
        completion_reason: row.get(28)?,
        retry_count: row_u32(row, 29)?,
        fallback_count: row_u32(row, 30)?,
        fallback_reason: row.get(31)?,
        upstream_authority: row.get(32)?,
        upstream_request_id: row.get(33)?,
        upstream_protocol: row.get(34)?,
        protocol_bridge: row.get(35)?,
        first_byte_source: row.get(36)?,
        subagent: row.get(37)?,
        upstream_error_summary: row.get(38)?,
        codex_session_id: row.get(39)?,
        codex_session_is_parent: row.get(40)?,
    })
}

fn normalize_query_value(
    value: &mut Option<String>,
    max_bytes: usize,
    label: &str,
) -> anyhow::Result<()> {
    let Some(current) = value.take() else {
        return Ok(());
    };
    let current = current.trim();
    if current.is_empty() {
        return Ok(());
    }
    if current.len() > max_bytes {
        anyhow::bail!("{label}不能超过 {max_bytes} 字节");
    }
    *value = Some(current.to_string());
    Ok(())
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    row.get::<_, i64>(index)
        .map(|value| u64::try_from(value).unwrap_or_default())
}

fn row_optional_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)
        .map(|value| value.map(|value| u64::try_from(value).unwrap_or_default()))
}

fn row_u32(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u32> {
    row.get::<_, i64>(index)
        .map(|value| u32::try_from(value).unwrap_or_default())
}

fn row_optional_u16(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u16>> {
    row.get::<_, Option<i64>>(index)
        .map(|value| value.map(|value| u16::try_from(value).unwrap_or_default()))
}

fn observe_terminal_value(entry: &mut PendingEntry, value: &Value) {
    let event_type = value.get("type").and_then(Value::as_str);
    let response_status = value
        .pointer("/response/status")
        .or_else(|| value.get("status"))
        .and_then(Value::as_str);
    apply_terminal_status(entry, event_type.or(response_status));
    if entry.error_code.is_none() {
        entry.error_code = first_string(
            value,
            &[
                "/response/error/code",
                "/response/error/type",
                "/error/code",
                "/error/type",
                "/code",
            ],
        )
        .map(bounded_string);
    }
}

fn apply_terminal_status(entry: &mut PendingEntry, terminal: Option<&str>) {
    let status = match terminal {
        Some("response.completed" | "completed") => Some(RequestStatus::Succeeded),
        Some("response.failed" | "failed" | "error") => Some(RequestStatus::Failed),
        Some("response.incomplete" | "incomplete") => Some(RequestStatus::Incomplete),
        _ => None,
    };
    if let Some(status) = status {
        entry.status = Some(status);
        entry.completion_reason = Some(
            match status {
                RequestStatus::Succeeded => "completed",
                RequestStatus::Failed => "failed",
                RequestStatus::Incomplete => "incomplete",
                RequestStatus::Cancelled => "cancelled",
            }
            .to_string(),
        );
    }
}

fn merge_usage(target: &mut RequestTokenUsage, value: &Value) {
    let Some(usage) = usage_value(value) else {
        return;
    };
    target.input_tokens = first_u64(usage, &["/input_tokens", "/prompt_tokens", "/inputTokens"])
        .or(target.input_tokens);
    target.output_tokens = first_u64(
        usage,
        &["/output_tokens", "/completion_tokens", "/outputTokens"],
    )
    .or(target.output_tokens);
    target.cached_input_tokens = first_u64(
        usage,
        &[
            "/input_tokens_details/cached_tokens",
            "/prompt_tokens_details/cached_tokens",
            "/cache_read_input_tokens",
            "/cache_read_tokens",
            "/cached_input_tokens",
        ],
    )
    .or(target.cached_input_tokens);
    target.cache_creation_input_tokens = first_u64(
        usage,
        &[
            "/cache_creation_input_tokens",
            "/cache_creation_tokens",
            "/cache_write_input_tokens",
        ],
    )
    .or(target.cache_creation_input_tokens);
    target.reasoning_output_tokens = first_u64(
        usage,
        &[
            "/output_tokens_details/reasoning_tokens",
            "/completion_tokens_details/reasoning_tokens",
            "/reasoning_tokens",
        ],
    )
    .or(target.reasoning_output_tokens);
    target.total_tokens = first_u64(usage, &["/total_tokens", "/totalTokens"])
        .or_else(|| match (target.input_tokens, target.output_tokens) {
            (Some(input), Some(output)) => Some(input.saturating_add(output)),
            _ => None,
        })
        .or(target.total_tokens);
}

fn usage_value(value: &Value) -> Option<&Value> {
    value
        .pointer("/response/usage")
        .or_else(|| value.get("usage"))
        .or_else(|| value.pointer("/message/usage"))
}

fn first_u64(value: &Value, pointers: &[&str]) -> Option<u64> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_u64))
}

fn first_string<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a str> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn bounded_string(value: &str) -> String {
    let value = value.trim();
    if value.len() <= MAX_LOG_STRING_BYTES {
        return value.to_string();
    }
    let mut end = MAX_LOG_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn unix_timestamp_ms_at(started_at: Instant) -> u64 {
    SystemTime::now()
        .checked_sub(started_at.elapsed())
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn elapsed_micros(started_at: Instant) -> u64 {
    started_at
        .elapsed()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn elapsed_millis(started_at: Instant) -> u64 {
    duration_millis(started_at.elapsed())
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn store_elapsed_once(target: &AtomicU64, started_at: Instant) {
    if target.load(Ordering::Relaxed) != 0 {
        return;
    }
    let elapsed = elapsed_micros(started_at).saturating_add(1);
    let _ = target.compare_exchange(0, elapsed, Ordering::Relaxed, Ordering::Relaxed);
}

fn load_duration_ms(value: &AtomicU64) -> Option<u64> {
    let encoded = value.load(Ordering::Relaxed);
    (encoded != 0).then_some(encoded.saturating_sub(1) / 1_000)
}

fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn open_private_append_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn ensure_private_sqlite_file(path: &Path) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let _file = options.open(path)?;
    #[cfg(unix)]
    {
        let mut permissions = _file.metadata()?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn ensure_sqlite_log_columns(connection: &Connection) -> rusqlite::Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(route_request_logs)")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !names.iter().any(|name| name == "usage_reported") {
        connection.execute_batch(
            "ALTER TABLE route_request_logs
                ADD COLUMN usage_reported INTEGER NOT NULL DEFAULT 0;",
        )?;
        if names.iter().any(|name| name == "usage_complete") {
            connection
                .execute_batch("UPDATE route_request_logs SET usage_reported = usage_complete;")?;
        }
    }
    if !names.iter().any(|name| name == "usage_unavailable_reason") {
        connection.execute_batch(
            "ALTER TABLE route_request_logs ADD COLUMN usage_unavailable_reason TEXT;",
        )?;
    }
    if !names.iter().any(|name| name == "upstream_error_summary") {
        connection.execute_batch(
            "ALTER TABLE route_request_logs ADD COLUMN upstream_error_summary TEXT;",
        )?;
    }
    if !names.iter().any(|name| name == "codex_session_id") {
        connection
            .execute_batch("ALTER TABLE route_request_logs ADD COLUMN codex_session_id TEXT;")?;
    }
    if !names.iter().any(|name| name == "codex_session_is_parent") {
        connection.execute_batch(
            "ALTER TABLE route_request_logs
                ADD COLUMN codex_session_is_parent INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

fn sqlite_table_has_column(connection: &Connection, column: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare("PRAGMA table_info(route_request_logs)")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn prune_sqlite_logs(connection: &Connection, retention_ms: u64) -> rusqlite::Result<usize> {
    let cutoff = unix_timestamp_ms().saturating_sub(retention_ms);
    connection.execute(
        "DELETE FROM route_request_logs WHERE timestamp_unix_ms < ?1",
        [to_i64(cutoff)],
    )
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(NDJSON_FILE_NAME));
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn rename_if_exists(source: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(request_id: &str) -> RouteRequestLogEntry {
        RouteRequestLogEntry {
            schema_version: SCHEMA_VERSION,
            request_id: request_id.to_string(),
            trace_id: request_id.to_string(),
            timestamp_unix_ms: 1,
            provider: Some("provider-a".to_string()),
            provider_name: Some("Provider A".to_string()),
            requested_model: "alias/model".to_string(),
            model: Some("model".to_string()),
            reasoning_effort: Some("high".to_string()),
            thinking_budget_tokens: None,
            ttft_ms: Some(12),
            upstream_header_ms: Some(3),
            total_duration_ms: 45,
            queue_delay_ms: 0,
            token_usage: RequestTokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cached_input_tokens: Some(2),
                cache_creation_input_tokens: None,
                reasoning_output_tokens: Some(1),
                total_tokens: Some(15),
            },
            usage_reported: true,
            usage_unavailable_reason: None,
            request_protocol: RequestProtocol::Sse,
            upstream_transport: Some(UpstreamTransport::HttpSse),
            request_kind: "responses".to_string(),
            status: RequestStatus::Succeeded,
            status_code: Some(200),
            upstream_status_code: Some(200),
            error_code: None,
            upstream_error_summary: None,
            completion_reason: Some("completed".to_string()),
            retry_count: 0,
            fallback_count: 0,
            fallback_reason: None,
            upstream_authority: Some("api.example.com".to_string()),
            upstream_request_id: Some("upstream-id".to_string()),
            upstream_protocol: Some("OpenAI Responses".to_string()),
            protocol_bridge: Some("Responses passthrough".to_string()),
            first_byte_source: Some(FirstByteSource::UpstreamHttpBody),
            client_fingerprint: None,
            subagent: false,
            codex_session_id: Some("thread-sample".to_string()),
            codex_session_is_parent: false,
        }
    }

    fn queued(entry: RouteRequestLogEntry) -> QueuedEntry {
        QueuedEntry {
            entry,
            enqueued_at: Instant::now(),
        }
    }

    #[test]
    fn extracts_responses_usage_and_terminal_status() {
        let value = serde_json::json!({
            "type":"response.completed",
            "response":{
                "status":"completed",
                "usage":{
                    "input_tokens":10,
                    "output_tokens":5,
                    "total_tokens":15,
                    "input_tokens_details":{"cached_tokens":2},
                    "output_tokens_details":{"reasoning_tokens":1}
                }
            }
        });
        let mut usage = RequestTokenUsage::default();
        merge_usage(&mut usage, &value);
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.cached_input_tokens, Some(2));
        assert_eq!(usage.reasoning_output_tokens, Some(1));
        assert_eq!(usage.total_tokens, Some(15));
    }

    #[test]
    fn protocol_enum_wire_values_are_backend_independent() {
        assert_eq!(
            serde_json::to_string(&RequestProtocol::Http).unwrap(),
            "\"http\""
        );
        assert_eq!(
            serde_json::to_string(&RequestProtocol::Sse).unwrap(),
            "\"sse\""
        );
        assert_eq!(
            serde_json::to_string(&RequestProtocol::WebSocket).unwrap(),
            "\"ws\""
        );
        assert_eq!(RequestProtocol::WebSocket.as_str(), "ws");
        assert_eq!(
            serde_json::to_string(&UpstreamTransport::WebSocket).unwrap(),
            "\"ws\""
        );
        assert_eq!(UpstreamTransport::WebSocket.as_str(), "ws");
    }

    #[test]
    fn ndjson_sink_batches_and_rotates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("requests.ndjson");
        let mut sink = NdjsonSink::open(path.clone(), 1_024 * 1_024, 2).unwrap();
        sink.write_batch(&[queued(sample_entry("one")), queued(sample_entry("two"))])
            .unwrap();
        sink.finish().unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(contents.lines().count(), 2);
        assert!(contents.contains("\"cachedInputTokens\":2"));
        assert!(contents.contains("\"codexSessionId\":\"thread-sample\""));
        assert!(!contents.contains("requestBody"));
    }

    #[test]
    fn ndjson_rotation_closes_and_reopens_the_active_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("requests.ndjson");
        let mut sink = NdjsonSink::open(path.clone(), 1, 2).unwrap();
        sink.write_batch(&[queued(sample_entry("one"))]).unwrap();
        sink.write_batch(&[queued(sample_entry("two"))]).unwrap();
        sink.finish().unwrap();

        assert!(path.exists());
        assert!(rotated_path(&path, 1).exists());
        assert!(fs::read_to_string(path).unwrap().contains("\"two\""));
    }

    #[test]
    fn sqlite_sink_inserts_one_row_per_request() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("requests.sqlite3");
        let mut sink = SqliteSink::open(path.clone(), 30).unwrap();
        sink.write_batch(&[queued(sample_entry("one")), queued(sample_entry("two"))])
            .unwrap();
        sink.finish().unwrap();
        drop(sink);
        let connection = Connection::open(path).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM route_request_logs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn sqlite_query_paginates_in_reverse_time_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SQLITE_FILE_NAME);
        let mut sink = SqliteSink::open(path, 30).unwrap();
        let mut first = sample_entry("first");
        first.timestamp_unix_ms = 100;
        let mut second = sample_entry("second");
        second.timestamp_unix_ms = 200;
        let mut third = sample_entry("third");
        third.timestamp_unix_ms = 300;
        sink.write_batch(&[queued(first), queued(second), queued(third)])
            .unwrap();
        sink.finish().unwrap();
        drop(sink);

        let first_page = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                page: 1,
                page_size: 2,
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(first_page.total, 3);
        assert_eq!(first_page.total_pages, 2);
        assert_eq!(
            first_page
                .items
                .iter()
                .map(|item| item.request_id.as_str())
                .collect::<Vec<_>>(),
            ["third", "second"]
        );

        let second_page = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                page: 2,
                page_size: 2,
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(second_page.items[0].request_id, "first");
    }

    #[test]
    fn sqlite_query_uses_bound_search_and_combined_filters() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SQLITE_FILE_NAME);
        let mut sink = SqliteSink::open(path, 30).unwrap();
        let mut matching = sample_entry("matching-request");
        matching.codex_session_id = Some("parent-session-42".into());
        matching.codex_session_is_parent = true;
        let mut other = sample_entry("other-request");
        other.provider = Some("provider-b".into());
        other.provider_name = Some("Provider B".into());
        other.model = Some("model-b".into());
        other.request_protocol = RequestProtocol::Http;
        other.status = RequestStatus::Failed;
        other.status_code = Some(500);
        other.upstream_error_summary = Some("provider quota exhausted".into());
        sink.write_batch(&[queued(matching), queued(other)])
            .unwrap();
        sink.finish().unwrap();
        drop(sink);

        let filtered = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                search: Some("matching".into()),
                provider: Some("PROVIDER-A".into()),
                model: Some("MODEL".into()),
                status: Some("SUCCEEDED".into()),
                protocol: Some("SSE".into()),
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.items[0].request_id, "matching-request");

        let error_match = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                search: Some("quota exhausted".into()),
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(error_match.total, 1);
        assert_eq!(
            error_match.items[0].upstream_error_summary.as_deref(),
            Some("provider quota exhausted")
        );

        let session_match = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                search: Some("session-42".into()),
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(session_match.total, 1);
        assert_eq!(
            session_match.items[0].codex_session_id.as_deref(),
            Some("parent-session-42")
        );
        assert!(session_match.items[0].codex_session_is_parent);

        let injected = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                search: Some("%' OR 1=1 --".into()),
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(injected.total, 0);
    }

    #[test]
    fn query_missing_sqlite_is_empty_and_ndjson_is_explicitly_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let missing = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery::default(),
        )
        .unwrap();
        assert_eq!(missing.status, "ok");
        assert!(missing.queryable);
        assert_eq!(missing.total, 0);
        assert!(missing.items.is_empty());

        let ndjson = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Ndjson,
            RouteRequestLogQuery::default(),
        )
        .unwrap();
        assert_eq!(ndjson.status, "unavailable");
        assert!(!ndjson.queryable);
        assert_eq!(ndjson.reason, Some("ndjson_not_queryable"));
    }

    #[test]
    fn query_legacy_sqlite_without_optional_columns_returns_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SQLITE_FILE_NAME);
        let mut sink = SqliteSink::open(path.clone(), 30).unwrap();
        sink.write_batch(&[queued(sample_entry("legacy"))]).unwrap();
        sink.finish().unwrap();
        drop(sink);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE route_request_logs DROP COLUMN upstream_error_summary;
                 ALTER TABLE route_request_logs DROP COLUMN codex_session_id;
                 ALTER TABLE route_request_logs DROP COLUMN codex_session_is_parent;",
            )
            .unwrap();
        drop(connection);

        let page = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery::default(),
        )
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].upstream_error_summary, None);
        assert_eq!(page.items[0].codex_session_id, None);
        assert!(!page.items[0].codex_session_is_parent);

        let searched = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                search: Some("legacy".into()),
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(searched.total, 1);
    }

    #[test]
    fn query_rejects_unbounded_or_invalid_parameters() {
        let directory = tempfile::tempdir().unwrap();
        for query in [
            RouteRequestLogQuery {
                page: 0,
                ..RouteRequestLogQuery::default()
            },
            RouteRequestLogQuery {
                page: MAX_QUERY_PAGE + 1,
                ..RouteRequestLogQuery::default()
            },
            RouteRequestLogQuery {
                page_size: MAX_QUERY_PAGE_SIZE + 1,
                ..RouteRequestLogQuery::default()
            },
            RouteRequestLogQuery {
                search: Some("x".repeat(MAX_QUERY_SEARCH_BYTES + 1)),
                ..RouteRequestLogQuery::default()
            },
            RouteRequestLogQuery {
                protocol: Some("ftp".into()),
                ..RouteRequestLogQuery::default()
            },
        ] {
            assert!(
                query_route_request_logs(directory.path(), RouteRequestLogBackend::Sqlite, query)
                    .is_err()
            );
        }
    }

    #[test]
    fn query_items_serialize_with_camel_case_fields() {
        let value = serde_json::to_value(RouteRequestLogQueryItem {
            request_id: "request".into(),
            trace_id: "trace".into(),
            timestamp_unix_ms: 1,
            provider: None,
            provider_name: None,
            requested_model: "requested".into(),
            model: None,
            reasoning_effort: None,
            thinking_budget_tokens: None,
            ttft_ms: None,
            upstream_header_ms: None,
            total_duration_ms: 1,
            queue_delay_ms: 0,
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
            cache_creation_input_tokens: None,
            reasoning_output_tokens: None,
            total_tokens: None,
            usage_reported: false,
            usage_unavailable_reason: None,
            request_protocol: "http".into(),
            upstream_transport: None,
            request_kind: "responses".into(),
            status: "succeeded".into(),
            status_code: Some(200),
            upstream_status_code: None,
            error_code: None,
            upstream_error_summary: Some("rate limit reached".into()),
            completion_reason: None,
            retry_count: 0,
            fallback_count: 0,
            fallback_reason: None,
            upstream_authority: None,
            upstream_request_id: None,
            upstream_protocol: None,
            protocol_bridge: None,
            first_byte_source: None,
            subagent: false,
            codex_session_id: Some("parent-thread".into()),
            codex_session_is_parent: true,
        })
        .unwrap();
        assert_eq!(value["requestId"], "request");
        assert_eq!(value["timestampUnixMs"], 1);
        assert_eq!(value["upstreamErrorSummary"], "rate limit reached");
        assert_eq!(value["codexSessionId"], "parent-thread");
        assert_eq!(value["codexSessionIsParent"], true);
        assert!(value.get("request_id").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_sink_uses_private_file_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("requests.sqlite3");
        let sink = SqliteSink::open(path.clone(), 30).unwrap();
        drop(sink);
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn sqlite_sink_prunes_expired_rows_during_normal_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("requests.sqlite3");
        let mut sink = SqliteSink::open(path.clone(), 1).unwrap();
        sink.write_batch(&[queued(sample_entry("expired"))])
            .unwrap();
        sink.next_prune_at = Instant::now();
        let mut fresh = sample_entry("fresh");
        fresh.timestamp_unix_ms = unix_timestamp_ms();
        sink.write_batch(&[queued(fresh)]).unwrap();
        sink.finish().unwrap();
        drop(sink);

        let connection = Connection::open(path).unwrap();
        let ids = connection
            .prepare("SELECT request_id FROM route_request_logs ORDER BY request_id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec!["fresh"]);
    }

    #[test]
    fn disabled_runtime_creates_no_queue_or_thread() {
        let config = RouteRequestLogConfig::default();
        let directory = tempfile::tempdir().unwrap();
        assert!(
            RouteRequestLogRuntime::start_at(&config, directory.path().to_path_buf())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn disabled_controller_skips_lazy_start_fields() {
        let controller = RouteRequestLogController::with_root(PathBuf::new());
        let evaluated = AtomicBool::new(false);
        let probe = controller.begin(|producer| {
            evaluated.store(true, Ordering::Relaxed);
            producer.begin(RouteRequestLogStart {
                request_id: "disabled",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Http,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: None,
                codex_session_is_parent: false,
            })
        });
        assert!(probe.is_none());
        assert!(!evaluated.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn controller_hot_enable_writes_and_disable_flushes_without_restart() {
        let directory = tempfile::tempdir().unwrap();
        let controller = RouteRequestLogController::with_root(directory.path().to_path_buf());
        let config = RouteRequestLogConfig {
            enabled: true,
            backend: RouteRequestLogBackend::Sqlite,
            ..RouteRequestLogConfig::default()
        };
        assert_eq!(
            controller.reconfigure(&config).await.unwrap(),
            RouteRequestLogReconfigure::Enabled
        );
        let probe = controller
            .begin(|producer| {
                producer.begin(RouteRequestLogStart {
                    request_id: "hot-enable",
                    started_at: Instant::now(),
                    request_protocol: RequestProtocol::Http,
                    request_kind: "responses",
                    requested_model: "model",
                    reasoning_effort: None,
                    thinking_budget_tokens: None,
                    codex_session_id: None,
                    codex_session_is_parent: false,
                })
            })
            .unwrap();
        probe.finish_success();

        let disabled = RouteRequestLogConfig::default();
        assert_eq!(
            controller.reconfigure(&disabled).await.unwrap(),
            RouteRequestLogReconfigure::Disabled
        );
        assert!(controller.begin(|_| unreachable!()).is_none());
        let connection = Connection::open(directory.path().join(SQLITE_FILE_NAME)).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM route_request_logs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn long_probe_does_not_block_controller_disable() {
        let directory = tempfile::tempdir().unwrap();
        let controller = RouteRequestLogController::with_root(directory.path().to_path_buf());
        let config = RouteRequestLogConfig {
            enabled: true,
            backend: RouteRequestLogBackend::Sqlite,
            shutdown_flush_timeout_ms: 250,
            ..RouteRequestLogConfig::default()
        };
        controller.reconfigure(&config).await.unwrap();
        let probe = controller
            .begin(|producer| {
                producer.begin(RouteRequestLogStart {
                    request_id: "long-probe",
                    started_at: Instant::now(),
                    request_protocol: RequestProtocol::WebSocket,
                    request_kind: "responses",
                    requested_model: "model",
                    reasoning_effort: None,
                    thinking_budget_tokens: None,
                    codex_session_id: None,
                    codex_session_is_parent: false,
                })
            })
            .unwrap();

        tokio::time::timeout(
            Duration::from_millis(500),
            controller.reconfigure(&RouteRequestLogConfig::default()),
        )
        .await
        .expect("long-lived probe blocked logger shutdown")
        .unwrap();
        probe.finish_success();
    }

    #[tokio::test]
    async fn failed_sink_readiness_never_publishes_a_producer() {
        let directory = tempfile::tempdir().unwrap();
        let invalid_root = directory.path().join("not-a-directory");
        File::create(&invalid_root).unwrap();
        let controller = RouteRequestLogController::with_root(invalid_root);
        let config = RouteRequestLogConfig {
            enabled: true,
            backend: RouteRequestLogBackend::Sqlite,
            ..RouteRequestLogConfig::default()
        };
        assert!(controller.reconfigure(&config).await.is_err());
        assert!(controller.begin(|_| unreachable!()).is_none());
    }

    #[test]
    fn full_queue_drops_without_backpressure_and_counts_the_drop() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let stats = Arc::new(RouteRequestLogStats::default());
        let producer = RouteRequestLogProducer {
            sender,
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::clone(&stats),
        };
        producer.submit(sample_entry("one"));
        producer.submit(sample_entry("two"));
        assert_eq!(stats.accepted.load(Ordering::Relaxed), 1);
        assert_eq!(stats.dropped_full.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn probe_submits_exactly_once_with_timing_and_usage() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let stats = Arc::new(RouteRequestLogStats::default());
        let producer = RouteRequestLogProducer {
            sender,
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats,
        };
        let probe = producer
            .begin(RouteRequestLogStart {
                request_id: "request-one",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Sse,
                request_kind: "responses",
                requested_model: "alias/model",
                reasoning_effort: Some("high"),
                thinking_budget_tokens: Some(4_096),
                codex_session_id: Some("thread-one"),
                codex_session_is_parent: false,
            })
            .unwrap();
        probe.resolve_route(
            "provider-a",
            "Provider A",
            "alias/model",
            "model",
            "api.example.com",
            "OpenAI Responses",
            "Responses passthrough",
            false,
        );
        probe.mark_upstream_send(UpstreamTransport::HttpSse);
        probe.mark_first_upstream_data(FirstByteSource::UpstreamHttpBody);
        probe.mark_upstream_error_summary("provider detail");
        probe.observe_event(&serde_json::json!({
            "type":"response.completed",
            "response":{"usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}
        }));
        probe.finish_success();
        probe.finish_cancelled();

        let queued = receiver.try_recv().unwrap();
        assert_eq!(queued.entry.request_id, "request-one");
        assert_eq!(queued.entry.status, RequestStatus::Succeeded);
        assert!(queued.entry.ttft_ms.is_some());
        assert_eq!(queued.entry.token_usage.total_tokens, Some(15));
        assert_eq!(queued.entry.codex_session_id.as_deref(), Some("thread-one"));
        assert!(!queued.entry.codex_session_is_parent);
        assert_eq!(
            queued.entry.upstream_error_summary.as_deref(),
            Some("provider detail")
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn deferred_finish_waits_for_observer_and_still_submits_exactly_once() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let producer = RouteRequestLogProducer {
            sender,
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::new(RouteRequestLogStats::default()),
        };
        let probe = producer
            .begin(RouteRequestLogStart {
                request_id: "deferred-finish",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Sse,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: None,
                codex_session_is_parent: false,
            })
            .unwrap();
        let observer = probe.defer_finish().unwrap();

        probe.finish_success();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        probe.observe_event(&serde_json::json!({
            "type":"response.completed",
            "response":{"usage":{"input_tokens":9,"output_tokens":4,"total_tokens":13}}
        }));
        drop(observer);

        let entry = receiver.try_recv().unwrap().entry;
        assert_eq!(entry.status, RequestStatus::Succeeded);
        assert_eq!(entry.token_usage.total_tokens, Some(13));
        probe.finish_cancelled();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn synthetic_response_created_does_not_set_ttft() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let producer = RouteRequestLogProducer {
            sender,
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::new(RouteRequestLogStats::default()),
        };
        let probe = producer
            .begin(RouteRequestLogStart {
                request_id: "synthetic-created",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::WebSocket,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: None,
                codex_session_is_parent: false,
            })
            .unwrap();
        probe.mark_upstream_send(UpstreamTransport::WebSocket);
        probe.observe_event(&serde_json::json!({"type":"response.created"}));
        probe.finish_success();

        let entry = receiver.try_recv().unwrap().entry;
        assert_eq!(entry.ttft_ms, None);
        assert_eq!(entry.status_code, Some(200));
    }

    #[test]
    fn retry_updates_effective_transport_without_resetting_ttft_origin() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let producer = RouteRequestLogProducer {
            sender,
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::new(RouteRequestLogStats::default()),
        };
        let probe = producer
            .begin(RouteRequestLogStart {
                request_id: "transport-fallback",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Sse,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: None,
                codex_session_is_parent: false,
            })
            .unwrap();
        probe.mark_upstream_send(UpstreamTransport::HttpSse);
        let original_start = *probe.shared.upstream_started_at.get().unwrap();
        probe.set_upstream_transport(UpstreamTransport::Http);
        assert_eq!(
            *probe.shared.upstream_started_at.get().unwrap(),
            original_start
        );
        probe.finish_success();

        let entry = receiver.try_recv().unwrap().entry;
        assert_eq!(entry.upstream_transport, Some(UpstreamTransport::Http));
        assert_eq!(entry.status_code, Some(200));
    }

    #[test]
    fn unavailable_usage_has_an_explicit_reason() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let producer = RouteRequestLogProducer {
            sender,
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::new(RouteRequestLogStats::default()),
        };
        let probe = producer
            .begin(RouteRequestLogStart {
                request_id: "usage-unavailable",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Http,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: None,
                codex_session_is_parent: false,
            })
            .unwrap();
        probe.mark_usage_unavailable("response_tap_limit_exceeded");
        probe.finish_success();

        let entry = receiver.try_recv().unwrap().entry;
        assert!(!entry.usage_reported);
        assert_eq!(
            entry.usage_unavailable_reason.as_deref(),
            Some("response_tap_limit_exceeded")
        );
    }

    #[test]
    fn observer_panic_is_caught_and_counted() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let stats = Arc::clone(&probe.shared.producer.stats);
        probe.shield(|| panic!("synthetic observer panic"));
        assert_eq!(stats.observer_panics.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn shutdown_flush_timeout_is_bounded() {
        let (done_tx, done_rx) = oneshot::channel();
        let stats = Arc::new(RouteRequestLogStats::default());
        let runtime = RouteRequestLogRuntime {
            done: Mutex::new(Some(done_rx)),
            worker: Mutex::new(None),
            shutdown: Mutex::new(Some(mpsc::channel().0)),
            accepting: Arc::new(AtomicBool::new(true)),
            shutdown_timeout: Duration::from_millis(20),
            stats: Arc::clone(&stats),
        };
        let started_at = Instant::now();
        runtime.stop().await;
        drop(done_tx);

        assert!(started_at.elapsed() < Duration::from_millis(250));
        assert_eq!(stats.shutdown_timeouts.load(Ordering::Relaxed), 1);
    }
}
