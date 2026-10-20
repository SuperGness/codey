//! 会话统计：原生 rollout 提供持久化累计，路由探针提供本次运行期间的请求信息。
//! 两种累计不可相加；找不到原生累计时，返回明确标注范围的运行期数据。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::route_request_log::{RouteRequestLogEntry, SessionGenerationTotals};

#[path = "conversation_stats_activity.rs"]
mod activity;
#[path = "conversation_stats_history.rs"]
mod history;
#[path = "conversation_stats_turn.rs"]
mod turn;
pub(crate) use history::snapshot_with_history;
pub(crate) use turn::{LastTurnStatsSnapshot, TurnRequestTotals, TurnWindow};

/// 单会话状态条口径只统计正式生成请求（Responses create）；压缩等其他请求
/// 仍会计入 Token 累计，但不刷新速度与请求计数。
pub(crate) const CONVERSATION_REQUEST_KIND: &str = "responses";

const MAX_TRACKED_SESSIONS: usize = 512;
const MAX_ACTIVE_REQUESTS: usize = 256;
const MAX_BOUNDARY_REQUESTS: usize = 256;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationStatsSnapshot {
    pub last_turn: Option<LastTurnStatsSnapshot>,
    /// 最近一次完成请求实际使用的模型；上游未回报时回退请求模型。
    pub model: Option<String>,
    /// 正式生成请求完成数（含失败）。
    pub requests: Option<u64>,
    /// 仍在途的请求数，用于页面区分生成中与静止状态。
    pub in_flight: Option<u32>,
    /// 最近一次正式请求的完成状态：succeeded/failed/incomplete/cancelled。
    pub last_status: Option<String>,
    /// 正式请求的输出速度（Token/s），来源、范围与记录时间由 speed 字段明确。
    pub output_tokens_per_second: Option<f64>,
    pub speed_source: Option<&'static str>,
    pub speed_scope: Option<&'static str>,
    pub speed_recorded_at_unix_ms: Option<u64>,
    pub speed_is_estimate: bool,
    /// 仅当前请求使用 waiting/streaming；历史记录不携带运行状态。
    pub speed_state: Option<&'static str>,
    pub average_output_tokens_per_second: Option<f64>,
    pub average_speed_is_estimate: bool,
    pub average_speed_source: Option<&'static str>,
    pub average_speed_scope: Option<&'static str>,
    pub average_speed_request_count: Option<u64>,
    /// 只供历史合并使用，不向页面暴露请求 ID 或内部累计。
    #[serde(skip)]
    average_runtime_totals: SessionGenerationTotals,
    #[serde(skip)]
    average_history_boundary: Option<RuntimeHistoryBoundary>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    /// 累计总 Token；仅统计上游实际回报的值，未回报不计入。
    pub total_tokens: Option<u64>,
    pub updated_at_unix_ms: u64,
    pub usage_source: &'static str,
    pub usage_scope: &'static str,
    pub requests_scope: Option<&'static str>,
    /// 原生会话的累计用户轮数，独立于 last_turn；无法可靠去重时保持未知。
    pub turn_count: Option<u64>,
    pub step_count: Option<u64>,
    pub model_duration_ms: Option<u64>,
    pub tool_duration_ms: Option<u64>,
    pub average_ttft_ms: Option<f64>,
    pub activity_source: Option<&'static str>,
    pub activity_scope: Option<&'static str>,
    pub step_source: Option<&'static str>,
    pub step_scope: Option<&'static str>,
    pub timing_source: Option<&'static str>,
    pub timing_scope: Option<&'static str>,
    pub tool_timing_source: Option<&'static str>,
    pub tool_timing_scope: Option<&'static str>,
    pub timing_request_count: Option<u64>,
    pub ttft_sample_count: Option<u64>,
}

#[derive(Debug, Default)]
struct SessionStats {
    turn: turn::TurnRuntime,
    model: Option<String>,
    requests: u64,
    active_requests: HashMap<String, ActiveRequestStats>,
    completed_generation: SessionGenerationTotals,
    history_boundary: Option<RuntimeHistoryBoundary>,
    last_status: Option<String>,
    output_tokens_per_second: Option<f64>,
    input_tokens: u64,
    input_known: bool,
    output_tokens: u64,
    output_known: bool,
    cached_input_tokens: u64,
    cached_known: bool,
    cache_write_input_tokens: u64,
    cache_write_known: bool,
    total_tokens: u64,
    total_known: bool,
    /// 单调淘汰序号；系统时钟粒度不足以区分连续写入时仍保证淘汰稳定。
    touched_seq: u64,
    updated_at_unix_ms: u64,
    model_duration_ms: u64,
    timing_request_count: u64,
    ttft_total_ms: u64,
    ttft_sample_count: u64,
}

/// 历史只取最早跟踪请求之前的记录，之后的请求全部由运行期累计负责。
/// 同毫秒 ID 有界保存；达到上限或时钟回退时宁可减少历史覆盖，也不重复统计。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeHistoryBoundary {
    pub timestamp_unix_ms: u64,
    pub request_ids: HashSet<String>,
    pub exclude_entire_millisecond: bool,
}

impl RuntimeHistoryBoundary {
    fn observe(boundary: &mut Option<Self>, timestamp: u64, request_id: &str) {
        if boundary
            .as_ref()
            .is_none_or(|value| timestamp < value.timestamp_unix_ms)
        {
            *boundary = Some(Self {
                timestamp_unix_ms: timestamp,
                request_ids: HashSet::new(),
                exclude_entire_millisecond: false,
            });
        }
        let boundary = boundary.as_mut().expect("boundary initialized");
        if timestamp == boundary.timestamp_unix_ms && !boundary.exclude_entire_millisecond {
            if boundary.request_ids.len() < MAX_BOUNDARY_REQUESTS {
                boundary.request_ids.insert(request_id.to_owned());
            } else if !boundary.request_ids.contains(request_id) {
                boundary.request_ids.clear();
                boundary.exclude_entire_millisecond = true;
            }
        }
    }
}

#[derive(Debug, Default)]
struct ActiveRequestStats {
    started_seq: u64,
    started_at_unix_ms: u64,
    upstream_started_at: Option<Instant>,
    sample: Option<LiveSpeedSample>,
    streaming: bool,
    recorded_at_unix_ms: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct LiveSpeedSample {
    pub output_tokens: f64,
    pub generation_started_at: Instant,
    pub is_estimate: bool,
}

impl LiveSpeedSample {
    fn speed_at(&self, now: Instant) -> Option<f64> {
        let elapsed = now.saturating_duration_since(self.generation_started_at);
        (elapsed >= Duration::from_millis(250)).then(|| self.output_tokens / elapsed.as_secs_f64())
    }
}

#[derive(Debug, Default)]
pub(crate) struct ConversationStatsRegistry {
    sessions: Mutex<HashMap<String, SessionStats>>,
    sequence: AtomicU64,
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 页面 DOM 与请求头里的本地会话标识可能带 `local:` 前缀，两边统一剥掉，
/// 避免同一会话前后缀不一致时统计落空。
fn normalize_session_id(value: &str) -> &str {
    value
        .trim()
        .strip_prefix("local:")
        .unwrap_or(value.trim())
        .trim()
}

impl ConversationStatsRegistry {
    /// 请求进入路由时调用；会话 id 已在请求头解析阶段确认存在。
    pub(crate) fn begin_request(
        &self,
        session_id: &str,
        request_id: &str,
        started_at_unix_ms: u64,
    ) {
        let session_id = normalize_session_id(session_id);
        if session_id.is_empty() {
            return;
        }
        let touched_seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut sessions = lock_unpoisoned(&self.sessions);
        let stats = sessions.entry(session_id.to_string()).or_default();
        stats.turn.begin(request_id, started_at_unix_ms);
        RuntimeHistoryBoundary::observe(
            &mut stats.history_boundary,
            started_at_unix_ms,
            request_id,
        );
        if stats.active_requests.len() >= MAX_ACTIVE_REQUESTS
            && !stats.active_requests.contains_key(request_id)
            && let Some(oldest) = stats
                .active_requests
                .iter()
                .min_by_key(|(_, request)| request.started_seq)
                .map(|(id, _)| id.clone())
            && let Some(removed) = stats.active_requests.remove(&oldest)
        {
            stats.turn.lost_active(removed.started_at_unix_ms);
        }
        stats
            .active_requests
            .entry(request_id.to_string())
            .or_insert(ActiveRequestStats {
                started_seq: touched_seq,
                started_at_unix_ms,
                ..Default::default()
            });
        stats.touched_seq = touched_seq;
        evict_overflow(&mut sessions);
    }

    /// 模型用时从真实上游发送开始，不包含路由准备和工具间隔。
    pub(crate) fn mark_request_upstream_started(
        &self,
        session_id: &str,
        request_id: &str,
        at: Instant,
    ) {
        let mut sessions = lock_unpoisoned(&self.sessions);
        if let Some(active) = sessions
            .get_mut(normalize_session_id(session_id))
            .and_then(|stats| stats.active_requests.get_mut(request_id))
        {
            active.upstream_started_at.get_or_insert(at);
        }
    }

    /// 只更新仍属于同一活动请求的数字摘要，不保存流式内容或累计 Token。
    pub(crate) fn update_request_speed(
        &self,
        session_id: &str,
        request_id: &str,
        sample: LiveSpeedSample,
        recorded_at_unix_ms: u64,
    ) {
        let mut sessions = lock_unpoisoned(&self.sessions);
        let Some(stats) = sessions.get_mut(normalize_session_id(session_id)) else {
            return;
        };
        let Some(active) = stats.active_requests.get_mut(request_id) else {
            return;
        };
        active.streaming = true;
        active.sample =
            (sample.output_tokens.is_finite() && sample.output_tokens > 0.0).then_some(sample);
        active.recorded_at_unix_ms = Some(recorded_at_unix_ms);
    }

    /// 请求收尾（成功/失败/取消/不完整都会收尾）时调用。session 不存在时
    /// 静默忽略，保持与 begin_request 的对称语义。
    pub(crate) fn finish_request(&self, entry: &RouteRequestLogEntry) {
        let Some(session_id) = entry.codex_session_id.as_deref().map(normalize_session_id) else {
            return;
        };
        if session_id.is_empty() {
            return;
        }
        let touched_seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut sessions = lock_unpoisoned(&self.sessions);
        let stats = sessions.entry(session_id.to_string()).or_default();
        stats.active_requests.remove(&entry.request_id);
        stats.touched_seq = touched_seq;
        let own_generation =
            entry.request_kind == CONVERSATION_REQUEST_KIND && !entry.codex_session_is_parent;
        if own_generation {
            stats.turn.finish(entry);
            RuntimeHistoryBoundary::observe(
                &mut stats.history_boundary,
                entry.timestamp_unix_ms,
                &entry.request_id,
            );
            if let Some(totals) = SessionGenerationTotals::from_entry(entry) {
                stats.completed_generation.add(totals);
            }
            stats.updated_at_unix_ms = entry.timestamp_unix_ms;
            stats.model = entry
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .or_else(|| {
                    let requested = entry.requested_model.trim();
                    (!requested.is_empty()).then_some(requested)
                })
                .map(str::to_string);
        }
        let usage = &entry.token_usage;
        if let Some(value) = usage.input_tokens {
            stats.input_tokens = stats.input_tokens.saturating_add(value);
            stats.input_known = true;
        }
        if let Some(value) = usage.output_tokens {
            stats.output_tokens = stats.output_tokens.saturating_add(value);
            stats.output_known = true;
        }
        if let Some(value) = usage.cached_input_tokens {
            stats.cached_input_tokens = stats.cached_input_tokens.saturating_add(value);
            stats.cached_known = true;
        }
        if let Some(value) = usage.cache_creation_input_tokens {
            stats.cache_write_input_tokens = stats.cache_write_input_tokens.saturating_add(value);
            stats.cache_write_known = true;
        }
        let total =
            usage
                .total_tokens
                .or_else(|| match (usage.input_tokens, usage.output_tokens) {
                    (Some(input), Some(output)) => Some(input.saturating_add(output)),
                    _ => None,
                });
        if let Some(value) = total {
            stats.total_tokens = stats.total_tokens.saturating_add(value);
            stats.total_known = true;
        }
        if own_generation {
            stats.requests = stats.requests.saturating_add(1);
            if let Some(duration) = entry
                .router_pre_upstream_ms
                .and_then(|pre| entry.total_duration_ms.checked_sub(pre))
            {
                stats.model_duration_ms = stats.model_duration_ms.saturating_add(duration);
                stats.timing_request_count = stats.timing_request_count.saturating_add(1);
            }
            // first-byte 可能只是 SSE 心跳，只有实际首条内容计时才是 TTFT。
            if let Some(ttft) = entry
                .downstream_first_content_ms
                .filter(|v| *v <= entry.total_duration_ms)
            {
                stats.ttft_total_ms = stats.ttft_total_ms.saturating_add(ttft);
                stats.ttft_sample_count = stats.ttft_sample_count.saturating_add(1);
            }
            stats.last_status = Some(entry.status.as_str().to_string());
            stats.output_tokens_per_second = entry.output_tokens_per_second;
        }
        evict_overflow(&mut sessions);
    }

    pub(crate) fn snapshot(&self, session_id: &str) -> Option<ConversationStatsSnapshot> {
        self.snapshot_at(session_id, Instant::now())
    }

    pub(crate) fn snapshot_with_turn(
        &self,
        session_id: &str,
        last_turn: Option<LastTurnStatsSnapshot>,
    ) -> Option<ConversationStatsSnapshot> {
        let last_turn = last_turn.map(|turn| {
            let mut sessions = lock_unpoisoned(&self.sessions);
            // 即便首个请求还未到达，也记住明确轮边界，后续完成只做数字累计。
            let stats = sessions
                .entry(normalize_session_id(session_id).to_owned())
                .or_default();
            stats.touched_seq = self.sequence.fetch_add(1, Ordering::Relaxed);
            let result = stats
                .turn
                .snapshot(turn, &stats.active_requests, Instant::now());
            evict_overflow(&mut sessions);
            result
        });
        let mut snapshot = self.snapshot(session_id)?;
        snapshot.last_turn = last_turn;
        Some(snapshot)
    }

    fn snapshot_at(&self, session_id: &str, now: Instant) -> Option<ConversationStatsSnapshot> {
        let session_id = normalize_session_id(session_id);
        if session_id.is_empty() {
            return None;
        }
        let touched_seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut sessions = lock_unpoisoned(&self.sessions);
        let stats = sessions.get_mut(session_id)?;
        stats.touched_seq = touched_seq;
        let active = stats
            .active_requests
            .values()
            .max_by_key(|request| request.started_seq);
        let speed = active.map_or(stats.output_tokens_per_second, |request| {
            request
                .sample
                .as_ref()
                .and_then(|sample| sample.speed_at(now))
        });
        let mut average_totals = stats.completed_generation;
        let mut average_estimated = false;
        for request in stats.active_requests.values() {
            if let Some(sample) = &request.sample
                && sample.speed_at(now).is_some()
            {
                average_totals.add(SessionGenerationTotals {
                    output_tokens: sample.output_tokens,
                    generation_ms: now
                        .saturating_duration_since(sample.generation_started_at)
                        .as_secs_f64()
                        * 1000.0,
                    requests: 1,
                });
                average_estimated |= sample.is_estimate;
            }
        }
        let average_speed = average_totals.speed();
        Some(ConversationStatsSnapshot {
            model: active.is_none().then(|| stats.model.clone()).flatten(),
            requests: Some(stats.requests),
            in_flight: Some(stats.active_requests.len().try_into().unwrap_or(u32::MAX)),
            last_status: active
                .is_none()
                .then(|| stats.last_status.clone())
                .flatten(),
            output_tokens_per_second: speed,
            speed_source: (active.is_some() || speed.is_some()).then_some("runtime"),
            speed_scope: if active.is_some() {
                Some("current_request")
            } else {
                speed.map(|_| "latest_request")
            },
            speed_recorded_at_unix_ms: active.map_or_else(
                || speed.map(|_| stats.updated_at_unix_ms),
                |request| request.recorded_at_unix_ms,
            ),
            speed_is_estimate: active.is_some_and(|request| {
                request
                    .sample
                    .as_ref()
                    .is_some_and(|sample| sample.is_estimate)
            }),
            speed_state: active.map(|request| {
                if request.streaming {
                    "streaming"
                } else {
                    "waiting"
                }
            }),
            average_output_tokens_per_second: average_speed,
            average_speed_is_estimate: average_speed.is_some() && average_estimated,
            average_speed_source: average_speed.map(|_| "runtime"),
            average_speed_scope: average_speed.map(|_| "runtime"),
            average_speed_request_count: average_speed.map(|_| average_totals.requests),
            average_runtime_totals: average_totals,
            average_history_boundary: stats.history_boundary.clone(),
            input_tokens: stats.input_known.then_some(stats.input_tokens),
            output_tokens: stats.output_known.then_some(stats.output_tokens),
            cached_input_tokens: stats.cached_known.then_some(stats.cached_input_tokens),
            cache_write_input_tokens: stats
                .cache_write_known
                .then_some(stats.cache_write_input_tokens),
            total_tokens: stats.total_known.then_some(stats.total_tokens),
            updated_at_unix_ms: if active.is_some() {
                0
            } else {
                stats.updated_at_unix_ms
            },
            usage_source: "runtime",
            usage_scope: "runtime",
            requests_scope: Some("runtime"),
            step_count: Some(stats.requests),
            activity_source: Some("runtime"),
            activity_scope: Some("runtime"),
            step_source: Some("runtime"),
            step_scope: Some("runtime"),
            model_duration_ms: (stats.timing_request_count > 0).then_some(stats.model_duration_ms),
            average_ttft_ms: (stats.ttft_sample_count > 0)
                .then(|| stats.ttft_total_ms as f64 / stats.ttft_sample_count as f64),
            timing_source: Some("runtime"),
            timing_scope: Some("runtime"),
            timing_request_count: Some(stats.timing_request_count),
            ttft_sample_count: Some(stats.ttft_sample_count),
            ..Default::default()
        })
    }
}

fn evict_overflow(sessions: &mut HashMap<String, SessionStats>) {
    while sessions.len() > MAX_TRACKED_SESSIONS {
        let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, stats)| stats.touched_seq)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        sessions.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_request_log::{RequestProtocol, RequestStatus, RequestTokenUsage};

    pub(super) fn sample_entry(session: Option<&str>, kind: &str) -> RouteRequestLogEntry {
        RouteRequestLogEntry {
            route_decision: None,
            upstream_attempts: Vec::new(),
            attempts_omitted: 0,
            requested_service_tier: None,
            service_tier: None,
            schema_version: 1,
            request_id: "request".into(),
            trace_id: "trace".into(),
            timestamp_unix_ms: 1_000,
            provider: None,
            provider_name: None,
            official_account_id: None,
            requested_model: "codey/gpt-5".into(),
            model: Some("gpt-5".into()),
            upstream_response_model: None,
            reasoning_effort: None,
            thinking_budget_tokens: None,
            ttft_ms: None,
            router_pre_upstream_ms: None,
            upstream_first_byte_ms: None,
            downstream_first_content_ms: None,
            upstream_header_ms: None,
            total_duration_ms: 1,
            queue_delay_ms: 0,
            concurrency_wait_ms: None,
            output_tokens_per_second: Some(87.5),
            token_usage: RequestTokenUsage {
                input_tokens: Some(1_000),
                output_tokens: Some(200),
                cached_input_tokens: Some(600),
                cache_creation_input_tokens: Some(50),
                reasoning_output_tokens: None,
                total_tokens: Some(1_200),
            },
            usage_reported: true,
            usage_unavailable_reason: None,
            request_protocol: RequestProtocol::Sse,
            upstream_transport: None,
            request_kind: kind.into(),
            status: RequestStatus::Succeeded,
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
            request_input_state: None,
            request_input_items: None,
            request_has_previous_response_id: None,
            request_bytes: None,
            upstream_input_state: None,
            upstream_input_items: None,
            upstream_has_previous_response_id: None,
            upstream_bytes: None,
            first_byte_source: None,
            client_fingerprint: None,
            subagent: false,
            codex_session_id: session.map(str::to_string),
            codex_session_is_parent: false,
        }
    }

    #[test]
    fn aggregates_usage_and_speed_per_session() {
        let registry = ConversationStatsRegistry::default();
        registry.begin_request("thread-a", "request", 1_000);
        registry.begin_request("thread-b", "request", 1_000);
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        registry.finish_request(&sample_entry(Some("thread-a"), "responses_compact"));

        let a = registry.snapshot("thread-a").unwrap();
        assert_eq!(a.in_flight, Some(0));
        assert_eq!(a.requests, Some(1));
        assert_eq!(a.model.as_deref(), Some("gpt-5"));
        assert_eq!(a.input_tokens, Some(2_000));
        assert_eq!(a.output_tokens, Some(400));
        assert_eq!(a.cached_input_tokens, Some(1_200));
        assert_eq!(a.cache_write_input_tokens, Some(100));
        assert_eq!(a.total_tokens, Some(2_400));
        assert_eq!(a.output_tokens_per_second, Some(87.5));

        let b = registry.snapshot("thread-b").unwrap();
        assert_eq!(b.in_flight, Some(1));
        assert_eq!(b.requests, Some(0));
        assert!(registry.snapshot("thread-c").is_none());
    }

    #[test]
    fn ignores_entries_without_session() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(None, "responses"));
        assert!(registry.snapshot("thread-a").is_none());
    }

    #[test]
    fn keeps_unreported_fields_as_none() {
        let registry = ConversationStatsRegistry::default();
        let mut entry = sample_entry(Some("thread-a"), "responses");
        entry.token_usage = RequestTokenUsage::default();
        entry.output_tokens_per_second = None;
        registry.finish_request(&entry);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.requests, Some(1));
        assert_eq!(snapshot.input_tokens, None);
        assert_eq!(snapshot.cached_input_tokens, None);
        assert_eq!(snapshot.total_tokens, None);
        assert_eq!(snapshot.output_tokens_per_second, None);
    }

    #[test]
    fn model_switch_updates_display_model_and_speed() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        let mut switched = sample_entry(Some("thread-a"), "responses");
        switched.model = Some("claude-sonnet".into());
        switched.output_tokens_per_second = Some(42.0);
        registry.finish_request(&switched);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.model.as_deref(), Some("claude-sonnet"));
        assert_eq!(snapshot.output_tokens_per_second, Some(42.0));
    }

    #[test]
    fn inherited_subagent_request_cannot_replace_parent_request_speed() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        let mut child = sample_entry(Some("thread-a"), "responses");
        child.codex_session_is_parent = true;
        child.model = Some("child-model".into());
        child.output_tokens_per_second = Some(500.0);
        registry.finish_request(&child);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.requests, Some(1));
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5"));
        assert_eq!(snapshot.output_tokens_per_second, Some(87.5));
    }

    #[test]
    fn new_request_and_unreported_speed_clear_previous_speed() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        registry.begin_request("thread-a", "request", 1_000);
        let active = registry.snapshot("thread-a").unwrap();
        assert_eq!(active.output_tokens_per_second, None);
        assert_eq!(active.model, None);
        assert_eq!(active.last_status, None);
        assert_eq!(active.updated_at_unix_ms, 0);
        let mut entry = sample_entry(Some("thread-a"), "responses");
        entry.output_tokens_per_second = None;
        entry.model = None;
        entry.requested_model.clear();
        registry.finish_request(&entry);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.output_tokens_per_second, None);
        assert_eq!(snapshot.model, None);
    }

    #[test]
    fn timing_uses_own_requests_and_actual_first_content_only() {
        let registry = ConversationStatsRegistry::default();
        let mut entry = sample_entry(Some("thread-a"), "responses");
        entry.total_duration_ms = 1200;
        entry.router_pre_upstream_ms = Some(200);
        entry.downstream_first_content_ms = Some(300);
        registry.finish_request(&entry);
        entry.downstream_first_content_ms = None;
        entry.ttft_ms = Some(1);
        registry.finish_request(&entry);
        entry.codex_session_is_parent = true;
        registry.finish_request(&entry);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.step_count, Some(2));
        assert_eq!(snapshot.turn_count, None);
        assert_eq!(snapshot.model_duration_ms, Some(2000));
        assert_eq!(snapshot.average_ttft_ms, Some(300.0));
        assert_eq!(snapshot.timing_request_count, Some(2));
        assert_eq!(snapshot.ttft_sample_count, Some(1));
        assert_eq!(snapshot.tool_duration_ms, None);
    }

    #[test]
    fn live_speed_is_isolated_by_request_and_never_enters_token_totals() {
        let registry = ConversationStatsRegistry::default();
        let now = Instant::now();
        let sample = |tokens, estimated| LiveSpeedSample {
            output_tokens: tokens,
            generation_started_at: now - Duration::from_secs(1),
            is_estimate: estimated,
        };
        registry.begin_request(" local:thread-a ", "old", 1_000);
        registry.update_request_speed("thread-a", "old", sample(25.0, true), 100);
        registry.begin_request("thread-a", "new", 1_000);
        let waiting = registry.snapshot("thread-a").unwrap();
        assert_eq!(waiting.in_flight, Some(2));
        assert_eq!(waiting.speed_scope, Some("current_request"));
        assert_eq!(waiting.speed_state, Some("waiting"));
        assert_eq!(waiting.output_tokens_per_second, None);
        registry.update_request_speed("thread-a", "new", sample(50.0, true), 200);
        registry.update_request_speed("thread-b", "new", sample(900.0, false), 200);
        let active = registry.snapshot_at("thread-a", now).unwrap();
        assert_eq!(active.output_tokens_per_second, Some(50.0));
        assert!(active.speed_is_estimate);
        assert_eq!(active.speed_state, Some("streaming"));
        assert_eq!(active.total_tokens, None);
        assert_eq!(active.output_tokens, None);
        let mut old = sample_entry(Some("thread-a"), "responses");
        old.request_id = "old".into();
        registry.finish_request(&old);
        let active = registry.snapshot_at("thread-a", now).unwrap();
        assert_eq!(active.in_flight, Some(1));
        assert_eq!(active.output_tokens_per_second, Some(50.0));
        registry.update_request_speed("thread-a", "old", sample(900.0, false), 300);
        let paused = registry
            .snapshot_at("thread-a", now + Duration::from_secs(1))
            .unwrap();
        assert_eq!(paused.output_tokens_per_second, Some(25.0));
        let mut new = old;
        new.request_id = "new".into();
        new.output_tokens_per_second = Some(60.0);
        registry.finish_request(&new);
        registry.update_request_speed("thread-a", "new", sample(900.0, true), 400);
        let completed = registry.snapshot("thread-a").unwrap();
        assert_eq!(completed.in_flight, Some(0));
        assert_eq!(completed.output_tokens_per_second, Some(60.0));
        assert_eq!(completed.speed_scope, Some("latest_request"));
        assert_eq!(completed.speed_state, None);
        assert!(!completed.speed_is_estimate);
    }

    fn completed_step(id: &str, output: Option<u64>, generation_ms: u64) -> RouteRequestLogEntry {
        let mut entry = sample_entry(Some("thread-average"), "responses");
        entry.request_id = id.into();
        entry.token_usage.output_tokens = output;
        entry.router_pre_upstream_ms = Some(100);
        entry.upstream_first_byte_ms = Some(200);
        entry.total_duration_ms = 300 + generation_ms;
        entry
    }

    #[test]
    fn session_average_weights_all_steps_and_does_not_reset_while_waiting() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&completed_step("a", Some(100), 1_000));
        registry.finish_request(&completed_step("b", Some(600), 3_000));
        registry.begin_request("thread-average", "c", 2_000);
        let snapshot = registry.snapshot("thread-average").unwrap();
        assert_eq!(snapshot.average_output_tokens_per_second, Some(175.0));
        assert_eq!(snapshot.average_speed_request_count, Some(2));
        assert_eq!(snapshot.average_speed_scope, Some("runtime"));
        assert!(!snapshot.average_speed_is_estimate);
        assert_eq!(snapshot.speed_state, Some("waiting"));
        let json = serde_json::to_value(snapshot).unwrap();
        assert!(json.get("averageHistoryBoundary").is_none());
        assert!(json.get("averageRuntimeTotals").is_none());
    }

    #[test]
    fn session_average_includes_live_estimate_pause_and_actual_completion_replacement() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&completed_step("a", Some(100), 1_000));
        let now = Instant::now();
        registry.begin_request("thread-average", "b", 2_000);
        registry.update_request_speed(
            "thread-average",
            "b",
            LiveSpeedSample {
                output_tokens: 300.0,
                generation_started_at: now - Duration::from_secs(1),
                is_estimate: true,
            },
            2_001,
        );
        let live = registry.snapshot_at("thread-average", now).unwrap();
        assert_eq!(live.average_output_tokens_per_second, Some(200.0));
        assert_eq!(live.average_speed_request_count, Some(2));
        assert!(live.average_speed_is_estimate);
        let paused = registry
            .snapshot_at("thread-average", now + Duration::from_secs(2))
            .unwrap();
        assert_eq!(paused.average_output_tokens_per_second, Some(100.0));
        let completed = completed_step("b", Some(600), 3_000);
        registry.finish_request(&completed);
        let done = registry.snapshot("thread-average").unwrap();
        assert_eq!(done.average_output_tokens_per_second, Some(175.0));
        assert_eq!(done.average_speed_request_count, Some(2));
        assert!(!done.average_speed_is_estimate);
        assert_eq!(done.output_tokens, Some(700));
    }

    #[test]
    fn session_average_sums_parallel_generation_windows_and_ignores_unknown_completion() {
        let registry = ConversationStatsRegistry::default();
        let now = Instant::now();
        for (id, output, seconds) in [("a", 50.0, 1), ("b", 100.0, 3)] {
            registry.begin_request("thread-average", id, 1_000);
            registry.update_request_speed(
                "thread-average",
                id,
                LiveSpeedSample {
                    output_tokens: output,
                    generation_started_at: now - Duration::from_secs(seconds),
                    is_estimate: true,
                },
                2_000,
            );
        }
        assert_eq!(
            registry
                .snapshot_at("thread-average", now)
                .unwrap()
                .average_output_tokens_per_second,
            Some(37.5)
        );
        registry.finish_request(&completed_step("a", Some(50), 1_000));
        registry.finish_request(&completed_step("b", None, 3_000));
        let completed = registry.snapshot("thread-average").unwrap();
        assert_eq!(completed.average_output_tokens_per_second, Some(50.0));
        assert_eq!(completed.average_speed_request_count, Some(1));
        assert!(!completed.average_speed_is_estimate);
    }

    #[test]
    fn session_average_accepts_short_and_zero_output_but_rejects_invalid_measurements() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&completed_step("zero", Some(0), 1));
        assert_eq!(
            registry
                .snapshot("thread-average")
                .unwrap()
                .average_output_tokens_per_second,
            Some(0.0)
        );
        registry.finish_request(&completed_step("short", Some(1), 9));
        for mut entry in [
            completed_step("missing-output", None, 100),
            completed_step("zero-window", Some(900), 0),
            completed_step("missing-time", Some(900), 100),
            completed_step("backwards", Some(900), 100),
            completed_step("parent", Some(900), 100),
            completed_step("compact", Some(900), 100),
        ] {
            match entry.request_id.as_str() {
                "missing-time" => entry.router_pre_upstream_ms = None,
                "backwards" => entry.upstream_first_byte_ms = Some(500),
                "parent" => entry.codex_session_is_parent = true,
                "compact" => entry.request_kind = "responses_compact".into(),
                _ => {}
            }
            registry.finish_request(&entry);
        }
        let snapshot = registry.snapshot("thread-average").unwrap();
        assert_eq!(snapshot.average_output_tokens_per_second, Some(100.0));
        assert_eq!(snapshot.average_speed_request_count, Some(2));
    }

    #[test]
    fn average_boundary_handles_equal_timestamps_clock_regression_and_evicted_sessions() {
        let registry = ConversationStatsRegistry::default();
        registry.begin_request("thread-average", "a", 2_000);
        registry.begin_request("thread-average", "b", 2_000);
        registry.begin_request("thread-average", "c", 3_000);
        let boundary = registry
            .snapshot("thread-average")
            .unwrap()
            .average_history_boundary
            .unwrap();
        assert_eq!(boundary.request_ids.len(), 2);
        registry.begin_request("thread-average", "earlier", 1_000);
        let mut earlier = completed_step("earlier", Some(10), 100);
        earlier.timestamp_unix_ms = 1_000;
        // 模拟会话淘汰后在途旧请求收尾，依旧用它原来的起始时间建立边界。
        lock_unpoisoned(&registry.sessions).remove("thread-average");
        registry.begin_request("thread-average", "later", 4_000);
        registry.finish_request(&earlier);
        let boundary = registry
            .snapshot("thread-average")
            .unwrap()
            .average_history_boundary
            .unwrap();
        assert_eq!(boundary.timestamp_unix_ms, 1_000);
        assert_eq!(boundary.request_ids, HashSet::from(["earlier".to_owned()]));
    }

    #[test]
    fn average_memory_is_bounded_for_long_sessions_and_many_active_requests() {
        let registry = ConversationStatsRegistry::default();
        for index in 0..10_000 {
            let mut step = completed_step(&format!("step-{index}"), Some(10), 100);
            step.timestamp_unix_ms = index;
            registry.finish_request(&step);
        }
        for index in 0..MAX_ACTIVE_REQUESTS + 10 {
            registry.begin_request("thread-average", &format!("active-{index}"), 0);
        }
        let sessions = lock_unpoisoned(&registry.sessions);
        let stats = sessions.get("thread-average").unwrap();
        assert_eq!(stats.completed_generation.requests, 10_000);
        assert_eq!(stats.active_requests.len(), MAX_ACTIVE_REQUESTS);
        let boundary = stats.history_boundary.as_ref().unwrap();
        assert!(boundary.exclude_entire_millisecond);
        assert!(boundary.request_ids.is_empty());
        drop(sessions);
        let started = Instant::now();
        for _ in 0..1_000 {
            assert_eq!(
                registry
                    .snapshot("thread-average")
                    .unwrap()
                    .average_speed_request_count,
                Some(10_000)
            );
        }
        eprintln!(
            "session average: 1000 snapshots with 10000 completed + 256 active summaries: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn evicts_oldest_sessions_beyond_cap() {
        let registry = ConversationStatsRegistry::default();
        for index in 0..MAX_TRACKED_SESSIONS + 4 {
            let key = format!("thread-{index}");
            registry.begin_request(&key, "request", 1_000);
            registry.finish_request(&sample_entry(Some(&key), "responses"));
        }
        assert!(registry.snapshot("thread-0").is_none());
        assert!(registry.snapshot("thread-4").is_some());
    }
}
