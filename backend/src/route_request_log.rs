//! 请求日志的数据契约与模块入口。
//! 采集和生命周期由 collector 管理，writer 负责落盘，query 负责读取与清理。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod collector;
mod diagnostics;
pub(crate) use diagnostics::*;
mod query;
#[cfg(test)]
mod test_support;
mod writer;

pub(crate) use collector::{
    RouteRequestLogController, RouteRequestLogGuard, RouteRequestLogProbe,
    RouteRequestLogReconfigure, is_output_delta_event, observe_upstream_response_model,
};
pub(crate) use query::{
    RouteRequestLogModelAliases, clear_route_request_log_files, last_turn_recorded_stats,
    query_route_request_log_models, query_route_request_log_stats_with_model_aliases,
    query_route_request_logs,
};
#[cfg(test)]
pub(crate) use query::{
    query_route_request_log_stats, session_recorded_average, session_recorded_timing,
};

const SCHEMA_VERSION: u8 = 14;
const MAX_LOG_STRING_BYTES: usize = 512;
const MAX_LOG_HEADERS_BYTES: usize = 16 * 1024;
pub(crate) const MAX_LOG_ERROR_BYTES: usize = 64 * 1024;

const NDJSON_FILE_NAME: &str = "route-requests.ndjson";
const SQLITE_FILE_NAME: &str = "route-requests.sqlite3";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// 输出不足 100 token，或上游首字节之后的响应窗口短于 100 毫秒时不计
/// 速度，避免短回复的网络突发导致统计失真。
const SPEED_MIN_OUTPUT_TOKENS: u64 = 100;
const SPEED_MIN_GENERATION_MS: u64 = 100;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RouteRequestLogCursor {
    pub timestamp_unix_ms: u64,
    pub request_id: String,
}

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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
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

/// Responses 请求体里 input 字段的形态。日志不保存正文，只保留这个摘要，
/// 用于区分缺失、null、空数组和正常输入。
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RequestInputState {
    Absent,
    Null,
    String,
    Object,
    Array,
    Other,
}

impl RequestInputState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Null => "null",
            Self::String => "string",
            Self::Object => "object",
            Self::Array => "array",
            Self::Other => "other",
        }
    }
}

/// 请求体形态摘要。正文、提示词和输出都不落盘，只记录形状与体积。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RequestBodySummary {
    pub input_state: RequestInputState,
    pub input_items: u64,
    pub has_previous_response_id: bool,
    pub bytes: Option<u64>,
}

impl RequestBodySummary {
    /// 只有 Responses 形态的请求体才有 input 语义；其他协议的上游请求体
    /// 不应套用该摘要。
    pub(crate) fn from_responses_body(body: &Value, bytes: Option<u64>) -> Self {
        let (input_state, input_items) = match body.get("input") {
            None => (RequestInputState::Absent, 0),
            Some(Value::Null) => (RequestInputState::Null, 0),
            Some(Value::Array(items)) => (RequestInputState::Array, items.len() as u64),
            Some(Value::String(_)) => (RequestInputState::String, 1),
            Some(Value::Object(_)) => (RequestInputState::Object, 1),
            Some(_) => (RequestInputState::Other, 0),
        };
        let has_previous_response_id = body
            .get("previous_response_id")
            .is_some_and(|value| !value.is_null());
        Self {
            input_state,
            input_items,
            has_previous_response_id,
            bytes,
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
    pub(crate) fn as_str(self) -> &'static str {
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

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogEntry {
    pub route_decision: Option<RouteDecision>,
    pub upstream_attempts: Vec<UpstreamAttemptLog>,
    pub attempts_omitted: u32,
    pub requested_service_tier: Option<String>,
    pub service_tier: Option<String>,
    pub schema_version: u8,
    pub request_id: String,
    pub trace_id: String,
    pub timestamp_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    /// 官方线路所属的账号记录 id。多条官方线路共用 provider 语义时，只有这
    /// 个字段能稳定区分账号，额度推算也按它分组。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_account_id: Option<String>,
    pub requested_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 上游响应里返回的实际使用模型，上游未回报时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router_pre_upstream_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_first_byte_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downstream_first_content_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_header_ms: Option<u64>,
    pub total_duration_ms: u64,
    pub queue_delay_ms: u64,
    /// 官方账号执行额度的等待时间，与日志写入队列延迟独立。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency_wait_ms: Option<u64>,
    /// 输出速度（上游响应阶段每秒输出 Token 数），无法计算时不记录。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens_per_second: Option<f64>,
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
    pub fallback_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_authority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_request_headers: Option<String>,
    /// 上游返回的响应头，敏感值同样只保留脱敏占位。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_response_headers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_bridge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_input_state: Option<RequestInputState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_input_items: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_has_previous_response_id: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_input_state: Option<RequestInputState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_input_items: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_has_previous_response_id: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_bytes: Option<u64>,
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
    pub from_unix_ms: Option<u64>,
    pub to_unix_ms: Option<u64>,
    pub cursor_mode: bool,
    pub cursor: Option<RouteRequestLogCursor>,
    pub request_id: Option<String>,
    pub request_kind: Option<String>,
    pub session_id: Option<String>,
    pub official_account_id: Option<String>,
    pub group_by: Option<String>,
    pub all_time: bool,
    pub group_sort: Option<String>,
    pub include_daily_trend: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
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
    pub next_cursor: Option<RouteRequestLogCursor>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogSummary {
    pub total: u64,
    pub succeeded_count: u64,
    pub failed_count: u64,
    pub incomplete_count: u64,
    pub cancelled_count: u64,
    pub avg_duration: Option<f64>,
    pub avg_ttft: Option<f64>,
    pub avg_output_tokens_per_second: Option<f64>,
    pub avg_router_pre_upstream: Option<f64>,
    pub avg_upstream_header: Option<f64>,
    pub avg_upstream_first_byte: Option<f64>,
    pub avg_downstream_first_content: Option<f64>,
    pub avg_queue_delay: Option<f64>,
    pub success_rate: Option<f64>,
    pub input_tokens_sum: Option<u64>,
    pub output_tokens_sum: Option<u64>,
    pub total_tokens_sum: Option<u64>,
    pub cached_tokens_sum: Option<u64>,
    pub usage_reported_count: u64,
    pub total_tokens_known_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogGroup {
    pub key: String,
    #[serde(flatten)]
    pub summary: RouteRequestLogSummary,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogTrend {
    pub timestamp_unix_ms: u64,
    pub total: u64,
    pub total_tokens_sum: Option<u64>,
    pub avg_duration: Option<f64>,
    pub avg_ttft: Option<f64>,
    pub avg_output_tokens_per_second: Option<f64>,
    pub avg_downstream_first_content: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogDailyTrend {
    pub timestamp_unix_ms: u64,
    pub total: u64,
    pub total_tokens_sum: Option<u64>,
    pub total_tokens_known_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogAnalytics {
    pub status: &'static str,
    pub backend: &'static str,
    pub queryable: bool,
    pub reason: Option<&'static str>,
    pub from_unix_ms: u64,
    pub to_unix_ms: u64,
    #[serde(flatten)]
    pub summary: RouteRequestLogSummary,
    pub groups: Vec<RouteRequestLogGroup>,
    pub groups_truncated: bool,
    pub trend: Vec<RouteRequestLogTrend>,
    pub daily_trend: Vec<RouteRequestLogDailyTrend>,
    pub bucket_ms: u64,
    pub database_bytes: u64,
    pub wal_bytes: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogQueryItem {
    pub route_decision: Option<RouteDecision>,
    pub upstream_attempts: Vec<UpstreamAttemptLog>,
    pub attempts_omitted: u32,
    pub requested_service_tier: Option<String>,
    pub service_tier: Option<String>,
    pub request_id: String,
    pub trace_id: String,
    pub timestamp_unix_ms: u64,
    pub provider: Option<String>,
    pub provider_name: Option<String>,
    /// 非官方请求为 None，官方请求记录该线路所属的账号记录 id。
    pub official_account_id: Option<String>,
    pub requested_model: String,
    pub model: Option<String>,
    /// 上游响应里返回的实际使用模型，上游未回报时为 None。
    pub upstream_response_model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub thinking_budget_tokens: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub router_pre_upstream_ms: Option<u64>,
    pub upstream_first_byte_ms: Option<u64>,
    pub downstream_first_content_ms: Option<u64>,
    pub upstream_header_ms: Option<u64>,
    pub total_duration_ms: u64,
    pub queue_delay_ms: u64,
    pub concurrency_wait_ms: Option<u64>,
    /// 输出速度（上游响应阶段每秒输出 Token 数），无法计算时为 None。
    pub output_tokens_per_second: Option<f64>,
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
    pub fallback_count: u32,
    pub fallback_reason: Option<String>,
    pub upstream_authority: Option<String>,
    pub upstream_request_headers: Option<String>,
    pub upstream_response_headers: Option<String>,
    pub upstream_request_id: Option<String>,
    pub upstream_protocol: Option<String>,
    pub protocol_bridge: Option<String>,
    /// 请求体形态摘要：日志只保留 input 的形态、项数、是否带
    /// previous_response_id 与字节数，不保存正文。
    pub request_input_state: Option<String>,
    pub request_input_items: Option<u64>,
    pub request_has_previous_response_id: Option<bool>,
    pub request_bytes: Option<u64>,
    pub upstream_input_state: Option<String>,
    pub upstream_input_items: Option<u64>,
    pub upstream_has_previous_response_id: Option<bool>,
    pub upstream_bytes: Option<u64>,
    pub first_byte_source: Option<String>,
    pub subagent: bool,
    pub codex_session_id: Option<String>,
    pub codex_session_is_parent: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogClearResult {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub removed_file_count: usize,
    pub removed_files: Vec<String>,
    pub recording_enabled: bool,
    pub recording_active: bool,
    pub recording_restarted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_error: Option<String>,
}

impl RouteRequestLogClearResult {
    fn empty(recording_enabled: bool) -> Self {
        Self {
            status: "ok",
            message: None,
            removed_file_count: 0,
            removed_files: Vec::new(),
            recording_enabled,
            recording_active: false,
            recording_restarted: false,
            error: None,
            restart_error: None,
        }
    }

    fn refresh_status(&mut self) {
        self.status = if self.error.is_none() && self.restart_error.is_none() {
            "ok"
        } else {
            "failed"
        };
        self.message = match (&self.error, &self.restart_error) {
            (Some(error), Some(restart_error)) => Some(format!("{error}；{restart_error}")),
            (Some(error), None) => Some(error.clone()),
            (None, Some(restart_error)) => Some(restart_error.clone()),
            (None, None) => None,
        };
    }

    pub(crate) fn failed(recording_enabled: bool, error: impl Into<String>) -> Self {
        let mut result = Self::empty(recording_enabled);
        result.error = Some(error.into());
        result.refresh_status();
        result
    }
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogHealth {
    pub enabled: bool,
    pub active: bool,
    pub sample_rate_per_million: u32,
    pub pending_entries: u64,
    #[serde(flatten)]
    pub stats: RouteRequestLogStatsSnapshot,
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

struct QueuedEntry {
    entry: RouteRequestLogEntry,
    enqueued_at: Instant,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SessionRecordedTiming {
    pub requests: u64,
    pub model_duration_ms: Option<u64>,
    pub average_ttft_ms: Option<f64>,
    pub timing_request_count: u64,
    pub ttft_sample_count: u64,
}

/// 会话平均速度使用同一批请求的输出量和生成时长，不对单步 TPS 求平均。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SessionGenerationTotals {
    pub output_tokens: f64,
    pub generation_ms: f64,
    pub requests: u64,
}

impl SessionGenerationTotals {
    pub(crate) fn add(&mut self, other: Self) {
        self.output_tokens += other.output_tokens;
        self.generation_ms += other.generation_ms;
        self.requests = self.requests.saturating_add(other.requests);
    }

    pub(crate) fn speed(self) -> Option<f64> {
        if self.requests == 0 || self.generation_ms <= 0.0 {
            return None;
        }
        let speed = self.output_tokens * 1000.0 / self.generation_ms;
        (speed.is_finite() && speed >= 0.0).then_some(speed)
    }

    pub(crate) fn from_entry(entry: &RouteRequestLogEntry) -> Option<Self> {
        let generation_ms = entry
            .total_duration_ms
            .checked_sub(entry.router_pre_upstream_ms?)?
            .checked_sub(entry.upstream_first_byte_ms?)?;
        (generation_ms > 0).then_some(Self {
            output_tokens: entry.token_usage.output_tokens? as f64,
            generation_ms: generation_ms as f64,
            requests: 1,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(crate) struct RouteRequestLogModelQuery {
    pub from_unix_ms: Option<u64>,
    pub to_unix_ms: Option<u64>,
    pub provider: Option<String>,
    pub official_account_id: Option<String>,
    pub after_model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteRequestLogModelPage {
    pub queryable: bool,
    pub models: Vec<String>,
    pub next_cursor: Option<String>,
}

fn unix_timestamp_ms() -> u64 {
    crate::fs_util::timestamp_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// 上游响应阶段每秒输出 Token 数。需要已知输出 Token 数和两段上游计时，
/// 且满足最小输出量与最短生成窗口；条件不足时返回 None。
fn output_tokens_per_second(
    output_tokens: Option<u64>,
    total_duration_ms: u64,
    router_pre_upstream_ms: Option<u64>,
    upstream_first_byte_ms: Option<u64>,
) -> Option<f64> {
    let output_tokens = output_tokens?;
    if output_tokens < SPEED_MIN_OUTPUT_TOKENS {
        return None;
    }
    let first_byte_ms = router_pre_upstream_ms?.checked_add(upstream_first_byte_ms?)?;
    let generation_ms = total_duration_ms.checked_sub(first_byte_ms)?;
    if generation_ms < SPEED_MIN_GENERATION_MS {
        return None;
    }
    Some(output_tokens as f64 * 1_000.0 / generation_ms as f64)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
