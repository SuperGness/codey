//! 请求元数据采集，以及 producer 和后台 writer 的启停协调。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwapOption;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use super::diagnostics::*;
use super::query::clear_route_request_log_files;
use super::writer::{BatchSink, WorkerConfig, WriterControl, writer_loop};
use super::{
    FirstByteSource, MAX_LOG_ERROR_BYTES, MAX_LOG_HEADERS_BYTES, MAX_LOG_STRING_BYTES, QueuedEntry,
    RequestBodySummary, RequestProtocol, RequestStatus, RequestTokenUsage,
    RouteRequestLogClearResult, RouteRequestLogEntry, RouteRequestLogHealth, RouteRequestLogStart,
    RouteRequestLogStats, RouteRequestLogStatsSnapshot, SCHEMA_VERSION, UpstreamTransport,
    duration_millis, lock_unpoisoned, output_tokens_per_second,
};
use crate::config::RouteRequestLogConfig;
use crate::conversation_stats::LiveSpeedSample;

const PARTS_PER_MILLION: u64 = 1_000_000;

#[derive(Clone)]
pub(crate) struct RouteRequestLogProducer {
    /// 落盘 producer 持有 writer 队列；会话统计 producer 为 None，只向
    /// `session_stats` 注册表写聚合结果。
    sender: Option<SyncSender<QueuedEntry>>,
    accepting: Arc<AtomicBool>,
    submitting: Arc<AtomicU64>,
    sample_rate_per_million: u32,
    sample_sequence: Arc<AtomicU64>,
    stats: Arc<RouteRequestLogStats>,
    session_stats: Option<Arc<crate::conversation_stats::ConversationStatsRegistry>>,
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
        // 纯统计 producer 不关心无会话标识的请求（如模型列表），直接短路，
        // 让这些请求保持与日志关闭时相同的零开销路径。
        if self.sender.is_none() && start.codex_session_id.is_none() {
            return None;
        }
        let sampled = self.should_sample();
        if !sampled {
            self.stats.sampled_out.fetch_add(1, Ordering::Relaxed);
            if self.session_stats.is_none() || start.codex_session_id.is_none() {
                return None;
            }
        }
        let request_id = bounded_string(start.request_id);
        let timestamp_unix_ms = unix_timestamp_ms_at(start.started_at);
        let session_request = start
            .codex_session_id
            .filter(|_| {
                start.request_kind == crate::conversation_stats::CONVERSATION_REQUEST_KIND
                    && !start.codex_session_is_parent
                    && self.session_stats.is_some()
            })
            .map(|session_id| (bounded_string(session_id), request_id.clone()));
        if let (Some(registry), Some((session_id, request_id))) =
            (&self.session_stats, &session_request)
        {
            registry.begin_request(session_id, request_id, timestamp_unix_ms);
        }
        let entry = PendingEntry {
            route_decision: None,
            upstream_attempts: Vec::new(),
            attempts_omitted: 0,
            active_attempt_started: None,
            requested_service_tier: None,
            service_tier: None,
            request_body: None,
            upstream_body: None,
            request_id: request_id.clone(),
            trace_id: request_id,
            timestamp_unix_ms,
            provider: None,
            provider_name: None,
            official_account_id: None,
            requested_model: bounded_string(start.requested_model),
            model: None,
            upstream_response_model: None,
            downstream_response_model: None,
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
            fallback_count: 0,
            fallback_reason: None,
            upstream_authority: None,
            upstream_request_headers: None,
            upstream_response_headers: None,
            upstream_request_id: None,
            upstream_protocol: None,
            protocol_bridge: None,
            first_byte_source: None,
            client_fingerprint: None,
            subagent: false,
            codex_session_id: start.codex_session_id.map(bounded_string),
            codex_session_is_parent: start.codex_session_is_parent,
        };
        // 日志采样只控制落盘，页面会话统计始终采集；未采样探针没有 writer 队列。
        let mut producer = self.clone();
        if !sampled {
            producer.sender = None;
        }
        Some(RouteRequestLogProbe {
            shared: Arc::new(ProbeShared {
                producer,
                started_at: start.started_at,
                upstream_started_at: OnceLock::new(),
                first_byte_micros: AtomicU64::new(0),
                router_pre_upstream_micros: AtomicU64::new(0),
                concurrency_wait_micros: AtomicU64::new(0),
                downstream_first_content_micros: AtomicU64::new(0),
                upstream_header_micros: AtomicU64::new(0),
                finished: AtomicBool::new(false),
                finish_gate: Mutex::new(ProbeFinishGate::default()),
                session_request,
                live_output: Mutex::new(LiveOutputProgress::default()),
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
        // 会话统计先于队列开关检查记录：writer 停收时收尾请求仍会走 submit，
        // 必须结清在途计数，否则页面会一直显示“生成中”。
        if let Some(registry) = &self.session_stats {
            registry.finish_request(&entry);
        }
        if !self.accepting.load(Ordering::Acquire) {
            self.stats.dropped_closed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let Some(sender) = &self.sender else {
            return;
        };
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
        let result = sender.try_send(queued);
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
    router_pre_upstream_micros: AtomicU64,
    concurrency_wait_micros: AtomicU64,
    downstream_first_content_micros: AtomicU64,
    upstream_header_micros: AtomicU64,
    finished: AtomicBool,
    finish_gate: Mutex<ProbeFinishGate>,
    session_request: Option<(String, String)>,
    live_output: Mutex<LiveOutputProgress>,
    entry: Mutex<PendingEntry>,
}

const LIVE_SPEED_INTERVAL: Duration = Duration::from_millis(250);

/// 只保留计数和单调时间。字符估算不进入计费 Token，也不持久化。
#[derive(Default)]
struct LiveOutputProgress {
    first_output_at: Option<Instant>,
    last_published_at: Option<Instant>,
    estimated_quarters: u64,
    reported_baseline: Option<(u64, u64)>,
}

impl LiveOutputProgress {
    fn observe(
        &mut self,
        quarters: u64,
        reported_tokens: Option<u64>,
        observed_at: Instant,
        upstream_first_byte_at: Option<Instant>,
    ) -> Option<LiveSpeedSample> {
        let reported_tokens = reported_tokens.filter(|tokens| *tokens > 0);
        if quarters == 0 && reported_tokens.is_none() {
            return None;
        }
        self.first_output_at.get_or_insert(observed_at);
        self.estimated_quarters = self.estimated_quarters.saturating_add(quarters);
        if let Some(tokens) = reported_tokens
            && self
                .reported_baseline
                .is_none_or(|(previous, _)| tokens > previous)
        {
            self.reported_baseline = Some((tokens, self.estimated_quarters));
        }
        if self
            .last_published_at
            .is_some_and(|last| observed_at.saturating_duration_since(last) < LIVE_SPEED_INTERVAL)
        {
            return None;
        }
        self.last_published_at = Some(observed_at);
        let start = upstream_first_byte_at.or(self.first_output_at)?;
        let (tokens, estimated) = match self.reported_baseline {
            Some((tokens, baseline)) => {
                let additional = self.estimated_quarters.saturating_sub(baseline);
                (
                    tokens as f64 + additional as f64 / 4.0,
                    additional > 0 || upstream_first_byte_at.is_none(),
                )
            }
            None => (self.estimated_quarters as f64 / 4.0, true),
        };
        Some(LiveSpeedSample {
            output_tokens: tokens,
            generation_started_at: start,
            is_estimate: estimated,
        })
    }
}

pub(crate) fn is_output_delta_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.function_call_arguments.delta"
            | "response.custom_tool_call_input.delta"
    )
}

/// ASCII 约四字符一个 Token，其他 Unicode 码点约一个；只用于标明的估算速度。
fn estimated_token_quarters(text: &str) -> u64 {
    text.chars().fold(0_u64, |total, ch| {
        total.saturating_add(if ch.is_ascii() { 1 } else { 4 })
    })
}

#[derive(Default)]
struct ProbeFinishGate {
    observers: usize,
    pending: Option<(RequestStatus, &'static str)>,
    response_duration_ms: Option<u64>,
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
    route_decision: Option<RouteDecision>,
    upstream_attempts: Vec<UpstreamAttemptLog>,
    attempts_omitted: u32,
    active_attempt_started: Option<Instant>,
    requested_service_tier: Option<String>,
    service_tier: Option<String>,
    request_body: Option<RequestBodySummary>,
    upstream_body: Option<RequestBodySummary>,
    request_id: String,
    trace_id: String,
    timestamp_unix_ms: u64,
    provider: Option<String>,
    provider_name: Option<String>,
    official_account_id: Option<String>,
    requested_model: String,
    model: Option<String>,
    /// 上游响应里返回的实际使用模型，只在原始上游响应里取值。
    upstream_response_model: Option<String>,
    /// 下游 JSON 中出现的模型值。适配线路会在发给 Codex 的响应里带上
    /// 本地发送的模型名，所以这一来源只在上游没有回报时兜底。
    downstream_response_model: Option<String>,
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
    fallback_count: u32,
    fallback_reason: Option<String>,
    upstream_authority: Option<String>,
    upstream_request_headers: Option<String>,
    upstream_response_headers: Option<String>,
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
    pub(crate) fn records_diagnostics(&self) -> bool {
        self.shared.producer.sender.is_some()
    }

    pub(crate) fn record_route_decision(&self, decision: RouteDecision) {
        if self.records_diagnostics() {
            self.shield(|| lock_unpoisoned(&self.shared.entry).route_decision = Some(decision));
        }
    }

    /// 只在真正启动上游请求时调用；WebSocket 握手不算请求尝试。
    pub(crate) fn begin_upstream_attempt(
        &self,
        transport: UpstreamTransport,
        trigger: AttemptTrigger,
    ) {
        self.mark_upstream_send(transport);
        if !self.records_diagnostics() {
            return;
        }
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            finish_attempt(&mut entry, AttemptOutcome::Superseded, None);
            if entry.upstream_attempts.len() >= MAX_RECORDED_ATTEMPTS {
                entry.attempts_omitted = entry.attempts_omitted.saturating_add(1);
                return;
            }
            let trigger = if trigger == AttemptTrigger::Initial
                && entry
                    .fallback_reason
                    .as_deref()
                    .is_some_and(|reason| reason.starts_with("websocket_"))
            {
                AttemptTrigger::TransportFallback
            } else {
                trigger
            };
            let sequence = entry.upstream_attempts.len() as u32 + 1;
            entry.upstream_attempts.push(UpstreamAttemptLog {
                sequence,
                transport,
                trigger,
                started_ms: elapsed_millis(self.shared.started_at),
                duration_ms: None,
                header_ms: None,
                status_code: None,
                outcome: AttemptOutcome::Pending,
                phase: AttemptPhase::Send,
            });
            entry.active_attempt_started = Some(Instant::now());
        });
    }

    /// 与最终响应头字段分开，失败后成功的重试也保留前一次状态。
    pub(crate) fn record_attempt_headers(&self, status: u16) {
        if !self.records_diagnostics() {
            return;
        }
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            let Some(started) = entry.active_attempt_started else {
                return;
            };
            if let Some(attempt) = entry.upstream_attempts.last_mut() {
                attempt.status_code = Some(status);
                attempt
                    .header_ms
                    .get_or_insert_with(|| elapsed_millis(started));
                attempt.phase = AttemptPhase::ResponseBody;
            }
        });
    }

    pub(crate) fn finish_upstream_attempt(&self, outcome: AttemptOutcome, phase: AttemptPhase) {
        if self.records_diagnostics() {
            self.shield(|| {
                finish_attempt(
                    &mut lock_unpoisoned(&self.shared.entry),
                    outcome,
                    Some(phase),
                )
            });
        }
    }

    pub(crate) fn mark_attempt_phase(&self, phase: AttemptPhase) {
        if !self.records_diagnostics() {
            return;
        }
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            if entry.active_attempt_started.is_some()
                && let Some(attempt) = entry.upstream_attempts.last_mut()
            {
                attempt.phase = phase;
            }
        });
    }

    #[cfg(test)]
    pub(crate) fn attempts_for_test(&self) -> Vec<UpstreamAttemptLog> {
        lock_unpoisoned(&self.shared.entry)
            .upstream_attempts
            .clone()
    }

    pub(crate) fn set_requested_service_tier(&self, tier: Option<&str>) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).requested_service_tier = tier.map(bounded_string);
        });
    }

    pub(crate) fn observe_service_tier(&self, tier: &str) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).service_tier = Some(bounded_string(tier));
        });
    }

    /// 记录原始上游响应里返回的实际使用模型。上游通常在每个事件里重复
    /// 返回同一个值，只保留最先出现的非空值。
    pub(crate) fn observe_upstream_response_model(&self, model: &str) {
        self.shield(|| {
            let mut entry = lock_unpoisoned(&self.shared.entry);
            if entry.upstream_response_model.is_some() {
                return;
            }
            let model = bounded_string(model);
            if !model.is_empty() {
                entry.upstream_response_model = Some(model);
            }
        });
    }

    /// 记录客户端请求体的形态摘要，不保存正文。
    pub(crate) fn record_request_body(&self, summary: RequestBodySummary) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).request_body = Some(summary);
        });
    }

    /// 记录最终发往上游的请求体形态摘要。适配线路重写编码体时会再次调用，
    /// 保留最后一次发送的实际形态。
    pub(crate) fn record_upstream_body(&self, summary: RequestBodySummary) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).upstream_body = Some(summary);
        });
    }

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
            sender: Some(sender),
            accepting: Arc::new(AtomicBool::new(true)),
            submitting: Arc::new(AtomicU64::new(0)),
            sample_rate_per_million: 1_000_000,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::new(RouteRequestLogStats::default()),
            session_stats: None,
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
    pub(crate) fn service_tier_for_test(&self) -> Option<String> {
        lock_unpoisoned(&self.shared.entry).service_tier.clone()
    }

    #[cfg(test)]
    pub(crate) fn token_usage_for_test(&self) -> RequestTokenUsage {
        lock_unpoisoned(&self.shared.entry).token_usage.clone()
    }

    #[cfg(test)]
    pub(crate) fn upstream_response_model_for_test(&self) -> Option<String> {
        let entry = lock_unpoisoned(&self.shared.entry);
        entry
            .upstream_response_model
            .clone()
            .or_else(|| entry.downstream_response_model.clone())
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

    #[cfg(test)]
    pub(crate) fn downstream_content_observed_for_test(&self) -> bool {
        self.shared
            .downstream_first_content_micros
            .load(Ordering::Relaxed)
            != 0
    }

    #[cfg(test)]
    pub(crate) fn concurrency_wait_for_test(&self) -> Option<u64> {
        load_duration_ms(&self.shared.concurrency_wait_micros)
    }

    fn shield(&self, operation: impl FnOnce()) {
        let _ = self.shared.producer.catch_observer_panic(operation);
    }

    pub(crate) fn set_request_protocol(&self, protocol: RequestProtocol) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).request_protocol = protocol;
        });
    }

    /// 客户端来源不依赖线路是否解析成功，也不记录原始请求头。
    pub(crate) fn set_subagent(&self, subagent: bool) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).subagent = subagent;
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_route(
        &self,
        provider: &str,
        provider_name: &str,
        official_account_id: Option<&str>,
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
            entry.official_account_id = official_account_id.map(bounded_string);
            entry.requested_model = bounded_string(requested_model);
            entry.model = Some(bounded_string(model));
            entry.upstream_authority = Some(bounded_string(upstream_authority));
            entry.upstream_protocol = Some(bounded_string(upstream_protocol));
            entry.protocol_bridge = Some(bounded_string(protocol_bridge));
            entry.subagent = subagent;
        });
    }

    pub(crate) fn set_upstream_request_headers(&self, headers: &str) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).upstream_request_headers =
                Some(bounded_string_to(headers, MAX_LOG_HEADERS_BYTES));
        });
    }

    /// 记录上游返回的响应头。适配线路与 WebSocket 握手各自写入一次，
    /// 保留最后一次看到的值。
    pub(crate) fn set_upstream_response_headers(&self, headers: &str) {
        self.shield(|| {
            lock_unpoisoned(&self.shared.entry).upstream_response_headers =
                Some(bounded_string_to(headers, MAX_LOG_HEADERS_BYTES));
        });
    }

    pub(crate) fn mark_upstream_send(&self, transport: UpstreamTransport) {
        self.shield(|| {
            store_elapsed_once(
                &self.shared.router_pre_upstream_micros,
                self.shared.started_at,
            );
            let started = *self.shared.upstream_started_at.get_or_init(Instant::now);
            if let (Some(registry), Some((session, request))) = (
                &self.shared.producer.session_stats,
                &self.shared.session_request,
            ) {
                registry.mark_request_upstream_started(session, request, started);
            }
            lock_unpoisoned(&self.shared.entry).upstream_transport = Some(transport);
        });
    }

    pub(crate) fn record_concurrency_wait(&self, elapsed: Duration) {
        self.shared.concurrency_wait_micros.store(
            (elapsed.as_micros().min(u64::MAX as u128) as u64).max(1),
            Ordering::Relaxed,
        );
    }

    pub(crate) fn mark_upstream_headers(
        &self,
        status_code: u16,
        upstream_request_id: Option<&str>,
    ) {
        self.record_attempt_headers(status_code);
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

    /// Records when the first user-visible content has been handed to the
    /// downstream response writer. This is intentionally independent of the
    /// upstream first byte: protocol adaptation may delay visible text.
    pub(crate) fn mark_first_downstream_content(&self) {
        if self
            .shared
            .downstream_first_content_micros
            .load(Ordering::Relaxed)
            == 0
        {
            self.mark_first_downstream_content_at(Instant::now());
        }
    }

    pub(crate) fn mark_first_downstream_content_at(&self, written_at: Instant) {
        self.shield(|| {
            let target = &self.shared.downstream_first_content_micros;
            if target.load(Ordering::Relaxed) == 0 {
                let elapsed: u64 = written_at
                    .saturating_duration_since(self.shared.started_at)
                    .as_micros()
                    .try_into()
                    .unwrap_or(u64::MAX);
                let _ = target.compare_exchange(
                    0,
                    elapsed.saturating_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
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
                let mut end = summary.len().min(MAX_LOG_ERROR_BYTES);
                while !summary.is_char_boundary(end) {
                    end -= 1;
                }
                let at_limit = summary.len() >= MAX_LOG_ERROR_BYTES;
                let mut summary = summary[..end].to_string();
                if at_limit {
                    summary.push_str("\n[错误内容达到记录上限，内容可能不完整]");
                }
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
            if !entry.token_usage.reported() && entry.usage_unavailable_reason.is_none() {
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
        self.observe_event_at(event, Instant::now());
    }

    pub(crate) fn observe_event_at(&self, event: &Value, observed_at: Instant) {
        self.shield(|| {
            let event_type = event.get("type").and_then(Value::as_str);
            if self.shared.session_request.is_some()
                && !self.shared.finished.load(Ordering::Acquire)
            {
                let quarters = event_type
                    .filter(|kind| is_output_delta_event(kind))
                    .and_then(|_| event.get("delta").and_then(Value::as_str))
                    .map(estimated_token_quarters)
                    .unwrap_or(0);
                let reported_output = usage_value(event).and_then(|usage| {
                    first_u64(
                        usage,
                        &["/output_tokens", "/completion_tokens", "/outputTokens"],
                    )
                });
                self.observe_live_output(quarters, reported_output, observed_at);
            }
            let contains_usage = usage_value(event).is_some();
            if !contains_usage
                && response_service_tier(event).is_none()
                && upstream_response_model(event).is_none()
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

    /// 原始 SSE 投影只传数字；written_at 保留真实接收时间，排除后台解析排队。
    pub(crate) fn observe_output_delta_estimate(&self, quarters: u64, written_at: Instant) {
        self.shield(|| self.observe_live_output(quarters, None, written_at));
    }

    fn observe_live_output(
        &self,
        quarters: u64,
        reported_tokens: Option<u64>,
        observed_at: Instant,
    ) {
        let Some((session_id, request_id)) = &self.shared.session_request else {
            return;
        };
        if self.shared.finished.load(Ordering::Acquire) {
            return;
        }
        let Some(registry) = &self.shared.producer.session_stats else {
            return;
        };
        let first_byte = self.shared.first_byte_micros.load(Ordering::Relaxed);
        let upstream_first_byte_at = self
            .shared
            .upstream_started_at
            .get()
            .filter(|_| first_byte != 0)
            .and_then(|start| {
                start.checked_add(Duration::from_micros(first_byte.saturating_sub(1)))
            });
        let mut progress = lock_unpoisoned(&self.shared.live_output);
        if let Some(sample) = progress.observe(
            quarters,
            reported_tokens,
            observed_at,
            upstream_first_byte_at,
        ) {
            registry.update_request_speed(
                session_id,
                request_id,
                sample,
                unix_timestamp_ms_at(observed_at),
            );
        }
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

    pub(crate) fn finish_failed(&self) {
        self.finish(RequestStatus::Failed, "upstream_response_failed");
    }

    fn finish(&self, default_status: RequestStatus, default_reason: &'static str) {
        self.shield(|| self.finish_inner(default_status, default_reason));
    }

    fn finish_inner(&self, default_status: RequestStatus, default_reason: &'static str) {
        let total_duration_ms = {
            let mut gate = lock_unpoisoned(&self.shared.finish_gate);
            // Background metadata parsing may finish later than the response.
            let duration = *gate
                .response_duration_ms
                .get_or_insert_with(|| elapsed_millis(self.shared.started_at));
            if gate.observers != 0 {
                gate.pending.get_or_insert((default_status, default_reason));
                return;
            }
            if self.shared.finished.swap(true, Ordering::AcqRel) {
                return;
            }
            duration
        };
        let mut pending = lock_unpoisoned(&self.shared.entry);
        let status = pending.status.unwrap_or(default_status);
        let attempt_status = if default_status == RequestStatus::Failed
            || (default_status == RequestStatus::Cancelled && status != RequestStatus::Failed)
        {
            default_status
        } else {
            status
        };
        let outcome = match attempt_status {
            RequestStatus::Succeeded => AttemptOutcome::Succeeded,
            RequestStatus::Incomplete => AttemptOutcome::Incomplete,
            RequestStatus::Cancelled => AttemptOutcome::Cancelled,
            RequestStatus::Failed => AttemptOutcome::Failed,
        };
        finish_attempt(&mut pending, outcome, None);
        if pending.completion_reason.is_none() {
            pending.completion_reason = Some(default_reason.to_string());
        }
        let token_usage = std::mem::take(&mut pending.token_usage);
        let usage_reported = token_usage.reported();
        // 下游可见内容可能受缓冲或协议转换影响，速度按上游首字节后的响应
        // 窗口近似统计，包含全部输出 token（含推理）和少量响应写尾耗时。
        let output_tokens_per_second = output_tokens_per_second(
            token_usage.output_tokens,
            total_duration_ms,
            load_duration_ms(&self.shared.router_pre_upstream_micros),
            load_duration_ms(&self.shared.first_byte_micros),
        );
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
            route_decision: pending.route_decision.take(),
            upstream_attempts: std::mem::take(&mut pending.upstream_attempts),
            attempts_omitted: pending.attempts_omitted,
            requested_service_tier: pending.requested_service_tier.take(),
            service_tier: pending.service_tier.take(),
            schema_version: SCHEMA_VERSION,
            request_id: std::mem::take(&mut pending.request_id),
            trace_id: std::mem::take(&mut pending.trace_id),
            timestamp_unix_ms: pending.timestamp_unix_ms,
            provider: pending.provider.take(),
            provider_name: pending.provider_name.take(),
            official_account_id: pending.official_account_id.take(),
            requested_model: std::mem::take(&mut pending.requested_model),
            model: pending.model.take(),
            upstream_response_model: pending
                .upstream_response_model
                .take()
                .or_else(|| pending.downstream_response_model.take()),
            reasoning_effort: pending.reasoning_effort.take(),
            thinking_budget_tokens: pending.thinking_budget_tokens,
            ttft_ms: load_duration_ms(&self.shared.first_byte_micros),
            router_pre_upstream_ms: load_duration_ms(&self.shared.router_pre_upstream_micros),
            upstream_first_byte_ms: load_duration_ms(&self.shared.first_byte_micros),
            downstream_first_content_ms: load_duration_ms(
                &self.shared.downstream_first_content_micros,
            ),
            upstream_header_ms: load_duration_ms(&self.shared.upstream_header_micros),
            total_duration_ms,
            queue_delay_ms: 0,
            concurrency_wait_ms: load_duration_ms(&self.shared.concurrency_wait_micros),
            output_tokens_per_second,
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
            fallback_count: pending.fallback_count,
            fallback_reason: pending.fallback_reason.take(),
            upstream_authority: pending.upstream_authority.take(),
            upstream_request_headers: pending.upstream_request_headers.take(),
            upstream_response_headers: pending.upstream_response_headers.take(),
            upstream_request_id: pending.upstream_request_id.take(),
            upstream_protocol: pending.upstream_protocol.take(),
            protocol_bridge: pending.protocol_bridge.take(),
            request_input_state: pending.request_body.map(|summary| summary.input_state),
            request_input_items: pending.request_body.map(|summary| summary.input_items),
            request_has_previous_response_id: pending
                .request_body
                .map(|summary| summary.has_previous_response_id),
            request_bytes: pending.request_body.and_then(|summary| summary.bytes),
            upstream_input_state: pending.upstream_body.map(|summary| summary.input_state),
            upstream_input_items: pending.upstream_body.map(|summary| summary.input_items),
            upstream_has_previous_response_id: pending
                .upstream_body
                .map(|summary| summary.has_previous_response_id),
            upstream_bytes: pending.upstream_body.and_then(|summary| summary.bytes),
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
    /// 常驻的会话统计 producer：请求日志关闭或重配置空窗期内仍负责采集
    /// 会话级 Token/缓存/速度，probe 开销与日志开启时相同。
    stats_producer: Arc<RouteRequestLogProducer>,
    session_stats: Arc<crate::conversation_stats::ConversationStatsRegistry>,
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

    pub(crate) fn with_root(root: PathBuf) -> Self {
        let session_stats =
            Arc::new(crate::conversation_stats::ConversationStatsRegistry::default());
        Self {
            active: ArcSwapOption::empty(),
            stats_producer: Arc::new(RouteRequestLogProducer {
                sender: None,
                accepting: Arc::new(AtomicBool::new(true)),
                submitting: Arc::new(AtomicU64::new(0)),
                sample_rate_per_million: PARTS_PER_MILLION as u32,
                sample_sequence: Arc::new(AtomicU64::new(0)),
                stats: Arc::new(RouteRequestLogStats::default()),
                session_stats: Some(Arc::clone(&session_stats)),
            }),
            session_stats,
            state: AsyncMutex::new(RouteRequestLogControllerState::default()),
            root,
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn session_stats(
        &self,
    ) -> Arc<crate::conversation_stats::ConversationStatsRegistry> {
        Arc::clone(&self.session_stats)
    }

    /// The disabled fast path is a single atomic load plus a session-id check
    /// inside the stats-only producer. The start closure is deliberately lazy
    /// so extracting optional observation fields is skipped entirely while
    /// logging is off.
    pub(crate) fn begin(
        &self,
        begin: impl FnOnce(&RouteRequestLogProducer) -> Option<RouteRequestLogProbe>,
    ) -> Option<RouteRequestLogProbe> {
        let producer = self
            .active
            .load_full()
            .unwrap_or_else(|| Arc::clone(&self.stats_producer));
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
        let session_stats = Arc::clone(&self.session_stats);
        let started = tokio::task::spawn_blocking(move || {
            RouteRequestLogRuntime::start_at(&worker_config, root, session_stats)
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

    /// Serializes clearing with reconfiguration, removes the active producer
    /// before stopping its writer, and only republishes a replacement producer
    /// after the new sink has completed its readiness handshake.
    pub(crate) async fn clear(&self) -> RouteRequestLogClearResult {
        let mut state = self.state.lock().await;
        let config = state.config.clone().unwrap_or_default();
        let desired_active = config.enabled && config.sample_rate_per_million > 0;

        self.active.store(None);
        if let Some(retiring) = state.runtime.take()
            && !retiring.stop().await
        {
            let mut result = RouteRequestLogClearResult::empty(desired_active);
            result.error = Some(
                "请求日志 writer 未能在配置的期限内停止；为避免与仍在退出的 writer 竞争，未删除日志文件"
                    .to_string(),
            );
            result.refresh_status();
            return result;
        }

        let root = self.root.clone();
        let mut result = match tokio::task::spawn_blocking(move || {
            clear_route_request_log_files(&root, desired_active)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let mut result = RouteRequestLogClearResult::empty(desired_active);
                result.error = Some(format!("请求日志清理任务异常退出：{error}"));
                result
            }
        };

        if desired_active {
            let worker_config = config.clone();
            let root = self.root.clone();
            let session_stats = Arc::clone(&self.session_stats);
            match tokio::task::spawn_blocking(move || {
                RouteRequestLogRuntime::start_at(&worker_config, root, session_stats)
            })
            .await
            {
                Ok(Ok(Some((producer, runtime)))) => {
                    self.active.store(Some(Arc::new(producer)));
                    state.runtime = Some(runtime);
                    result.recording_active = true;
                    result.recording_restarted = true;
                }
                Ok(Ok(None)) => {
                    result.restart_error =
                        Some("请求日志配置未启用 replacement writer".to_string());
                }
                Ok(Err(error)) => {
                    result.restart_error = Some(format!("恢复请求日志记录失败：{error:#}"));
                }
                Err(error) => {
                    result.restart_error =
                        Some(format!("恢复请求日志 writer 任务异常退出：{error}"));
                }
            }
        }
        result.refresh_status();
        result
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

    pub(crate) async fn health(&self) -> RouteRequestLogHealth {
        let state = self.state.lock().await;
        let config = state.config.as_ref();
        let stats = state
            .runtime
            .as_ref()
            .map(RouteRequestLogRuntime::stats_snapshot)
            .unwrap_or_default();
        RouteRequestLogHealth {
            enabled: config.is_some_and(|config| config.enabled),
            active: self
                .active
                .load_full()
                .is_some_and(|producer| producer.accepting.load(Ordering::Acquire)),
            sample_rate_per_million: config.map_or(0, |config| config.sample_rate_per_million),
            pending_entries: stats
                .accepted
                .saturating_sub(stats.entries_written)
                .saturating_sub(stats.write_dropped),
            stats,
        }
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
        session_stats: Arc<crate::conversation_stats::ConversationStatsRegistry>,
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
            sender: Some(sender),
            accepting: Arc::clone(&accepting),
            submitting: Arc::clone(&submitting),
            sample_rate_per_million: config.sample_rate_per_million,
            sample_sequence: Arc::new(AtomicU64::new(0)),
            stats: Arc::clone(&stats),
            session_stats: Some(session_stats),
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

    pub(crate) async fn stop(&self) -> bool {
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
        completed
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

fn response_service_tier(value: &Value) -> Option<&str> {
    value
        .pointer("/response/service_tier")
        .or_else(|| value.get("service_tier"))
        .and_then(Value::as_str)
}

/// 上游响应里报告的实际使用模型。Responses 事件放在 response 对象里，
/// Chat Completions 放在根级，Anthropic Messages 放在 message 对象里。
pub(crate) fn upstream_response_model(value: &Value) -> Option<&str> {
    value
        .pointer("/response/model")
        .or_else(|| value.pointer("/message/model"))
        .or_else(|| value.get("model"))
        .and_then(Value::as_str)
}

/// 记录上游原始响应里出现的实际使用模型。桥接线路不经过原始字节投影，
/// 由各适配器在解析上游响应时直接调用。
pub(crate) fn observe_upstream_response_model(probe: Option<&RouteRequestLogProbe>, value: &Value) {
    if let Some(probe) = probe
        && let Some(model) = upstream_response_model(value)
    {
        probe.observe_upstream_response_model(model);
    }
}

fn observe_terminal_value(entry: &mut PendingEntry, value: &Value) {
    if let Some(tier) = response_service_tier(value) {
        entry.service_tier = Some(bounded_string(tier));
    }
    if entry.downstream_response_model.is_none()
        && let Some(model) = upstream_response_model(value)
    {
        let model = bounded_string(model);
        if !model.is_empty() {
            entry.downstream_response_model = Some(model);
        }
    }
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

fn finish_attempt(entry: &mut PendingEntry, outcome: AttemptOutcome, phase: Option<AttemptPhase>) {
    let Some(started) = entry.active_attempt_started.take() else {
        return;
    };
    if let Some(attempt) = entry.upstream_attempts.last_mut() {
        attempt.duration_ms = Some(elapsed_millis(started));
        let terminal = attempt.outcome;
        attempt.outcome =
            if terminal == AttemptOutcome::Succeeded && outcome == AttemptOutcome::Failed {
                attempt.phase = AttemptPhase::DownstreamDelivery;
                AttemptOutcome::DeliveryFailed
            } else if terminal == AttemptOutcome::Failed {
                // 上游已明确失败，随后下游断开不能改写该次上游结果。
                terminal
            } else if outcome == AttemptOutcome::Cancelled {
                AttemptOutcome::Cancelled
            } else if terminal != AttemptOutcome::Pending {
                terminal
            } else if matches!(outcome, AttemptOutcome::Superseded | AttemptOutcome::Failed)
                && attempt.status_code.is_some_and(|status| status >= 400)
            {
                AttemptOutcome::Rejected
            } else {
                outcome
            };
        if let Some(phase) = phase {
            attempt.phase = phase;
        } else if matches!(
            outcome,
            AttemptOutcome::Succeeded | AttemptOutcome::Incomplete
        ) {
            attempt.phase = AttemptPhase::Complete;
        }
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
        if entry.active_attempt_started.is_some()
            && let Some(attempt) = entry.upstream_attempts.last_mut()
        {
            attempt.outcome = match status {
                RequestStatus::Succeeded => AttemptOutcome::Succeeded,
                RequestStatus::Failed => AttemptOutcome::Failed,
                RequestStatus::Incomplete => AttemptOutcome::Incomplete,
                RequestStatus::Cancelled => AttemptOutcome::Cancelled,
            };
        }
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
            "/input_tokens_details/cache_write_tokens",
            "/prompt_tokens_details/cache_write_tokens",
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
    bounded_string_to(value, MAX_LOG_STRING_BYTES)
}

fn bounded_string_to(value: &str, max_bytes: usize) -> String {
    let value = value.trim();
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
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

#[cfg(test)]
#[path = "collector_tests.rs"]
mod tests;
