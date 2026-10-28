//! 最近用户轮次的数字摘要；内容与工具输出不进入此状态。
use super::{ActiveRequestStats, RuntimeHistoryBoundary};
use crate::route_request_log::{RouteRequestLogEntry, SessionGenerationTotals};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

const MAX_RECENT_REQUESTS: usize = 512;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LastTurnStatsSnapshot {
    pub id: String,
    pub started_at_unix_ms: Option<u64>,
    pub completed_at_unix_ms: Option<u64>,
    pub status: &'static str,
    pub step_count: Option<u64>,
    pub model_duration_ms: Option<u64>,
    pub tool_duration_ms: Option<u64>,
    pub average_ttft_ms: Option<f64>,
    pub average_output_tokens_per_second: Option<f64>,
    pub speed_is_estimate: bool,
    pub in_flight: Option<u32>,
    pub speed_state: Option<&'static str>,
    pub coverage: &'static str,
    #[serde(skip)]
    pub(super) runtime_totals: TurnRequestTotals,
    #[serde(skip)]
    pub(super) runtime_boundary: Option<RuntimeHistoryBoundary>,
    #[serde(skip)]
    pub(super) runtime_partial: bool,
    #[serde(skip)]
    pub(super) activity_partial: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnWindow {
    pub id: String,
    pub started_at_unix_ms: u64,
    pub completed_at_unix_ms: Option<u64>,
}

impl TurnWindow {
    pub(super) fn from_snapshot(snapshot: &LastTurnStatsSnapshot) -> Option<Self> {
        if snapshot.status != "running" && snapshot.completed_at_unix_ms.is_none() {
            return None;
        }
        Some(Self {
            id: snapshot.id.clone(),
            started_at_unix_ms: snapshot.started_at_unix_ms?,
            completed_at_unix_ms: snapshot.completed_at_unix_ms,
        })
    }

    pub(super) fn contains(&self, timestamp: u64) -> bool {
        timestamp > self.started_at_unix_ms
            && self.completed_at_unix_ms.is_none_or(|end| timestamp < end)
    }

    fn is_ambiguous_boundary(&self, timestamp: u64) -> bool {
        timestamp == self.started_at_unix_ms || self.completed_at_unix_ms == Some(timestamp)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TurnRequestTotals {
    pub requests: u64,
    pub excluded_boundary_requests: u64,
    pub generation: SessionGenerationTotals,
    pub model_duration_ms: u64,
    pub model_samples: u64,
    pub ttft_total_ms: f64,
    pub ttft_samples: u64,
}

impl TurnRequestTotals {
    fn from_entry(entry: &RouteRequestLogEntry) -> Self {
        let duration = entry
            .router_pre_upstream_ms
            .and_then(|pre| entry.total_duration_ms.checked_sub(pre));
        let ttft = entry
            .downstream_first_content_ms
            .filter(|ttft| *ttft <= entry.total_duration_ms);
        Self {
            requests: 1,
            excluded_boundary_requests: 0,
            generation: SessionGenerationTotals::from_entry(entry).unwrap_or_default(),
            model_duration_ms: duration.unwrap_or(0),
            model_samples: u64::from(duration.is_some()),
            ttft_total_ms: ttft.unwrap_or(0) as f64,
            ttft_samples: u64::from(ttft.is_some()),
        }
    }

    pub(crate) fn add(&mut self, other: Self) {
        self.requests = self.requests.saturating_add(other.requests);
        self.excluded_boundary_requests = self
            .excluded_boundary_requests
            .saturating_add(other.excluded_boundary_requests);
        self.generation.add(other.generation);
        self.model_duration_ms = self
            .model_duration_ms
            .saturating_add(other.model_duration_ms);
        self.model_samples = self.model_samples.saturating_add(other.model_samples);
        self.ttft_total_ms += other.ttft_total_ms;
        self.ttft_samples = self.ttft_samples.saturating_add(other.ttft_samples);
    }

    pub(super) fn apply(self, snapshot: &mut LastTurnStatsSnapshot) {
        // rollout 的 response ID 与路由 request ID 不同，不能相加；两个完整来源取最大覆盖。
        if self.requests > 0 {
            snapshot.step_count = Some(snapshot.step_count.unwrap_or(0).max(self.requests));
        }
        snapshot.model_duration_ms = (self.model_samples > 0).then_some(self.model_duration_ms);
        snapshot.average_ttft_ms =
            (self.ttft_samples > 0).then(|| self.ttft_total_ms / self.ttft_samples as f64);
        snapshot.average_output_tokens_per_second = self.generation.speed();
    }

    pub(super) fn incomplete(self) -> bool {
        self.excluded_boundary_requests > 0
            || self.requests > self.generation.requests
            || self.requests > self.model_samples
    }
}

#[derive(Debug)]
struct CompletedRequest {
    id: String,
    timestamp: u64,
    totals: TurnRequestTotals,
}

#[derive(Debug)]
struct CurrentTurn {
    window: TurnWindow,
    totals: TurnRequestTotals,
    boundary: Option<RuntimeHistoryBoundary>,
    partial: bool,
    latest_request_started_at: u64,
}

#[derive(Debug, Default)]
pub(super) struct TurnRuntime {
    recent: VecDeque<CompletedRequest>,
    dropped_through: Option<u64>,
    current: Option<CurrentTurn>,
}

impl TurnRuntime {
    pub(super) fn lost_active(&mut self, timestamp: u64) {
        self.dropped_through = Some(self.dropped_through.unwrap_or(0).max(timestamp));
        if let Some(current) = &mut self.current
            && (current.window.contains(timestamp)
                || current.window.is_ambiguous_boundary(timestamp))
        {
            current.partial = true;
        }
    }
    pub(super) fn begin(&mut self, id: &str, timestamp: u64) {
        if let Some(current) = &mut self.current
            && current.window.contains(timestamp)
        {
            RuntimeHistoryBoundary::observe(&mut current.boundary, timestamp, id);
        }
    }

    pub(super) fn finish(&mut self, entry: &RouteRequestLogEntry) {
        // 会话入口已统一去重，轮次累计无需重复扫描最近完成的请求。
        let totals = TurnRequestTotals::from_entry(entry);
        if let Some(current) = &mut self.current {
            if current.window.contains(entry.timestamp_unix_ms) {
                current.totals.add(totals);
                current.latest_request_started_at = current
                    .latest_request_started_at
                    .max(entry.timestamp_unix_ms);
                RuntimeHistoryBoundary::observe(
                    &mut current.boundary,
                    entry.timestamp_unix_ms,
                    &entry.request_id,
                );
            } else if current
                .window
                .is_ambiguous_boundary(entry.timestamp_unix_ms)
            {
                current.totals.excluded_boundary_requests =
                    current.totals.excluded_boundary_requests.saturating_add(1);
            }
        }
        self.recent.push_back(CompletedRequest {
            id: entry.request_id.clone(),
            timestamp: entry.timestamp_unix_ms,
            totals,
        });
        if self.recent.len() > MAX_RECENT_REQUESTS
            && let Some(dropped) = self.recent.pop_front()
        {
            self.dropped_through = Some(self.dropped_through.unwrap_or(0).max(dropped.timestamp));
        }
    }

    pub(super) fn snapshot(
        &mut self,
        mut snapshot: LastTurnStatsSnapshot,
        active: &HashMap<String, ActiveRequestStats>,
        now: Instant,
    ) -> LastTurnStatsSnapshot {
        let Some(window) = TurnWindow::from_snapshot(&snapshot) else {
            return snapshot;
        };
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.window.started_at_unix_ms > window.started_at_unix_ms)
        {
            // 并发轮询晚返回的旧文件快照不能回退当前轮的数字累计。
            return snapshot;
        }
        let rebuild = self.current.as_ref().is_none_or(|current| {
            current.window.id != window.id
                || current.window.started_at_unix_ms != window.started_at_unix_ms
                // 新上界同毫秒也无法归属；没有排除已累计请求时保留长轮数字累计。
                || (current.window.completed_at_unix_ms != window.completed_at_unix_ms
                    && (current.window.completed_at_unix_ms.is_some()
                        || window.completed_at_unix_ms.is_some_and(|end| current.latest_request_started_at >= end)))
        });
        if rebuild {
            let mut totals = TurnRequestTotals::default();
            let mut boundary = None;
            let mut latest_request_started_at = window.started_at_unix_ms;
            for request in &self.recent {
                if window.contains(request.timestamp) {
                    totals.add(request.totals);
                    latest_request_started_at = latest_request_started_at.max(request.timestamp);
                    RuntimeHistoryBoundary::observe(&mut boundary, request.timestamp, &request.id);
                } else if window.is_ambiguous_boundary(request.timestamp) {
                    totals.excluded_boundary_requests =
                        totals.excluded_boundary_requests.saturating_add(1);
                }
            }
            self.current = Some(CurrentTurn {
                partial: self
                    .dropped_through
                    .is_some_and(|timestamp| timestamp >= window.started_at_unix_ms),
                window: window.clone(),
                totals,
                boundary,
                latest_request_started_at,
            });
        }
        let current = self.current.as_mut().expect("turn initialized");
        current.window = window.clone();
        let mut totals = current.totals;
        let mut in_flight = 0u32;
        let mut streaming = false;
        let mut ambiguous_active = false;
        for (id, request) in active {
            if !window.contains(request.started_at_unix_ms) {
                ambiguous_active |= window.is_ambiguous_boundary(request.started_at_unix_ms);
                continue;
            }
            RuntimeHistoryBoundary::observe(&mut current.boundary, request.started_at_unix_ms, id);
            in_flight = in_flight.saturating_add(1);
            streaming |= request.streaming;
            if let Some(started) = request.upstream_started_at {
                totals.model_duration_ms = totals.model_duration_ms.saturating_add(
                    now.saturating_duration_since(started)
                        .as_millis()
                        .min(u64::MAX as u128) as u64,
                );
                totals.model_samples = totals.model_samples.saturating_add(1);
            }
            if let Some(sample) = &request.sample
                && sample.speed_at(now).is_some()
            {
                let elapsed = now.saturating_duration_since(sample.generation_started_at);
                totals.generation.add(SessionGenerationTotals {
                    output_tokens: sample.output_tokens,
                    generation_ms: elapsed.as_secs_f64() * 1000.0,
                    requests: 1,
                });
                snapshot.speed_is_estimate |= sample.is_estimate;
            }
        }
        snapshot.in_flight = Some(in_flight);
        snapshot.speed_state = (snapshot.status == "running").then_some(if streaming {
            "streaming"
        } else {
            "waiting"
        });
        snapshot.runtime_totals = totals;
        snapshot.runtime_boundary = current.boundary.clone();
        snapshot.runtime_partial = current.partial
            || ambiguous_active
            || current.totals.incomplete()
            || current
                .boundary
                .as_ref()
                .is_some_and(|boundary| boundary.exclude_entire_millisecond);
        totals.apply(&mut snapshot);
        snapshot.coverage = if snapshot.runtime_partial || snapshot.activity_partial {
            "partial"
        } else if totals.requests > 0 || in_flight > 0 {
            "runtime"
        } else {
            "unknown"
        };
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_stats::{ConversationStatsRegistry, LiveSpeedSample};
    use std::time::Duration;

    fn turn(id: &str, start: u64) -> LastTurnStatsSnapshot {
        LastTurnStatsSnapshot {
            id: id.into(),
            started_at_unix_ms: Some(start),
            status: "running",
            step_count: Some(0),
            coverage: "unknown",
            ..Default::default()
        }
    }

    fn step(id: &str, timestamp: u64, output: Option<u64>, duration: u64) -> RouteRequestLogEntry {
        let mut entry = crate::conversation_stats::tests::sample_entry(Some("own"), "responses");
        entry.request_id = id.into();
        entry.timestamp_unix_ms = timestamp;
        entry.router_pre_upstream_ms = Some(100);
        entry.upstream_first_byte_ms = Some(200);
        entry.total_duration_ms = duration + 300;
        entry.downstream_first_content_ms = Some(300);
        entry.token_usage.output_tokens = output;
        entry
    }

    fn snapshot(
        registry: &ConversationStatsRegistry,
        turn: LastTurnStatsSnapshot,
    ) -> LastTurnStatsSnapshot {
        registry
            .snapshot_with_turn("own", Some(turn))
            .unwrap()
            .last_turn
            .unwrap()
    }

    #[test]
    fn last_turn_recovers_before_first_poll_and_weights_multiple_steps() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&step("old", 100, Some(9000), 100));
        registry.finish_request(&step("a", 201, Some(100), 1000));
        registry.finish_request(&step("b", 300, Some(600), 3000));
        let latest = snapshot(&registry, turn("new", 200));
        assert_eq!(latest.average_output_tokens_per_second, Some(175.0));
        assert_eq!(latest.model_duration_ms, Some(4400));
        assert_eq!(latest.step_count, Some(2));
        assert_eq!(latest.average_ttft_ms, Some(300.0));
        registry.begin_request("own", "next-step", 400);
        assert_eq!(
            snapshot(&registry, turn("new", 200)).average_output_tokens_per_second,
            Some(175.0)
        );
        let next = snapshot(&registry, turn("next-user", 500));
        assert_eq!(next.average_output_tokens_per_second, None);
        assert_eq!(next.in_flight, Some(0));
        assert_eq!(next.speed_state, Some("waiting"));
        registry.finish_request(&step("next-step", 400, Some(999), 100));
        assert_eq!(
            snapshot(&registry, turn("next-user", 500)).model_duration_ms,
            None
        );
    }

    #[test]
    fn last_turn_live_estimate_replaced_by_actual_or_unknown_usage() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&step("a", 201, Some(100), 1000));
        snapshot(&registry, turn("new", 200));
        registry.begin_request("own", "b", 300);
        registry.update_request_speed(
            "own",
            "b",
            LiveSpeedSample {
                output_tokens: 300.0,
                generation_started_at: Instant::now() - Duration::from_secs(1),
                is_estimate: true,
            },
            301,
        );
        let live = snapshot(&registry, turn("new", 200));
        assert!(live.speed_is_estimate);
        assert_eq!(live.speed_state, Some("streaming"));
        assert!(live.average_output_tokens_per_second.unwrap() > 199.0);
        registry.finish_request(&step("b", 300, None, 3000));
        let mut done = turn("new", 200);
        done.status = "aborted";
        done.completed_at_unix_ms = Some(500);
        let done = snapshot(&registry, done);
        assert!(!done.speed_is_estimate);
        assert_eq!(done.average_output_tokens_per_second, Some(100.0));
        assert_eq!(done.coverage, "partial");
        assert_eq!(done.speed_state, None);
    }

    #[test]
    fn last_turn_ignores_parent_compaction_duplicates_and_unknown_boundaries() {
        let registry = ConversationStatsRegistry::default();
        for id in ["own", "parent", "compact"] {
            let mut entry = step(id, 201, Some(100), 1000);
            if id == "parent" {
                entry.codex_session_is_parent = true;
            }
            if id == "compact" {
                entry.request_kind = "responses_compact".into();
            }
            registry.finish_request(&entry);
        }
        registry.finish_request(&step("own", 201, Some(100), 1000));
        assert_eq!(snapshot(&registry, turn("new", 200)).step_count, Some(1));
        let mut missing = turn("missing", 200);
        missing.started_at_unix_ms = None;
        assert_eq!(
            snapshot(&registry, missing).average_output_tokens_per_second,
            None
        );
        let mut no_end = turn("new", 200);
        no_end.status = "completed";
        assert_eq!(
            snapshot(&registry, no_end).average_output_tokens_per_second,
            None
        );
    }

    #[test]
    fn last_turn_long_running_turn_keeps_numeric_totals_after_ring_eviction_and_completion() {
        let registry = ConversationStatsRegistry::default();
        snapshot(&registry, turn("long", 100));
        for index in 0..10_000 {
            registry.finish_request(&step(&format!("step-{index}"), 101 + index, Some(10), 100));
        }
        let mut done = turn("long", 100);
        done.status = "completed";
        done.completed_at_unix_ms = Some(20_000);
        let done = snapshot(&registry, done);
        assert_eq!(done.step_count, Some(10_000));
        assert_eq!(done.average_output_tokens_per_second, Some(100.0));
        assert_eq!(done.coverage, "runtime");
        let sessions = super::super::lock_unpoisoned(&registry.sessions);
        assert_eq!(sessions["own"].turn.recent.len(), MAX_RECENT_REQUESTS);
    }

    #[test]
    fn last_turn_initial_ring_overflow_is_explicitly_partial() {
        let registry = ConversationStatsRegistry::default();
        for index in 0..MAX_RECENT_REQUESTS + 10 {
            registry.finish_request(&step(
                &format!("step-{index}"),
                101 + index as u64,
                Some(10),
                100,
            ));
        }
        let latest = snapshot(&registry, turn("long", 100));
        assert_eq!(latest.coverage, "partial");
        assert_eq!(latest.step_count, Some(MAX_RECENT_REQUESTS as u64));
    }

    #[test]
    fn last_turn_live_model_time_starts_at_upstream_and_completion_replaces_it() {
        let registry = ConversationStatsRegistry::default();
        registry.begin_request("own", "a", 200);
        assert_eq!(
            snapshot(&registry, turn("new", 100)).model_duration_ms,
            None
        );
        registry.mark_request_upstream_started("own", "a", Instant::now() - Duration::from_secs(2));
        let waiting = snapshot(&registry, turn("new", 100));
        assert!(waiting.model_duration_ms.unwrap() >= 2000);
        assert_eq!(waiting.average_output_tokens_per_second, None);
        assert_eq!(waiting.speed_state, Some("waiting"));
        registry.finish_request(&step("a", 200, Some(100), 3000));
        let done = snapshot(&registry, turn("new", 100));
        assert_eq!(done.model_duration_ms, Some(3200));
        assert_eq!(done.average_output_tokens_per_second, Some(100.0 / 3.0));
    }

    #[test]
    fn last_turn_same_millisecond_start_requests_are_excluded_and_marked_partial() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&step("previous-turn", 200, Some(9000), 100));
        registry.finish_request(&step("possibly-new-turn", 200, Some(100), 1000));
        let unknown = snapshot(&registry, turn("new", 200));
        assert_eq!(unknown.model_duration_ms, None);
        assert_eq!(unknown.average_output_tokens_per_second, None);
        assert_eq!(unknown.average_ttft_ms, None);
        assert_eq!(unknown.coverage, "partial");
        assert_eq!(unknown.runtime_totals.excluded_boundary_requests, 2);

        registry.finish_request(&step("unambiguous", 201, Some(300), 1000));
        let known = snapshot(&registry, turn("new", 200));
        assert_eq!(known.step_count, Some(1));
        assert_eq!(known.model_duration_ms, Some(1200));
        assert_eq!(known.average_output_tokens_per_second, Some(300.0));
        assert_eq!(known.coverage, "partial");
    }

    #[test]
    fn last_turn_completion_removes_previously_counted_end_millisecond() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&step("inside", 101, Some(100), 1000));
        registry.finish_request(&step("end-boundary", 300, Some(900), 1000));
        assert_eq!(
            snapshot(&registry, turn("new", 100)).average_output_tokens_per_second,
            Some(500.0)
        );
        let mut done = turn("new", 100);
        done.status = "completed";
        done.completed_at_unix_ms = Some(300);
        for _ in 0..2 {
            let result = snapshot(&registry, done.clone());
            assert_eq!(result.step_count, Some(1));
            assert_eq!(result.model_duration_ms, Some(1200));
            assert_eq!(result.average_output_tokens_per_second, Some(100.0));
            assert_eq!(result.coverage, "partial");
            assert_eq!(result.runtime_totals.excluded_boundary_requests, 1);
        }
    }

    #[test]
    fn last_turn_only_end_millisecond_request_stays_unknown_after_completion() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&step("end-boundary", 300, Some(900), 1000));
        snapshot(&registry, turn("new", 100));
        let mut done = turn("new", 100);
        done.status = "aborted";
        done.completed_at_unix_ms = Some(300);
        let result = snapshot(&registry, done);
        assert_eq!(result.model_duration_ms, None);
        assert_eq!(result.average_output_tokens_per_second, None);
        assert_eq!(result.coverage, "partial");
    }

    #[test]
    fn last_turn_end_boundary_rebuild_reports_evicted_summaries_as_partial() {
        let registry = ConversationStatsRegistry::default();
        snapshot(&registry, turn("long", 100));
        for index in 0..MAX_RECENT_REQUESTS + 10 {
            registry.finish_request(&step(
                &format!("step-{index}"),
                101 + index as u64,
                Some(10),
                100,
            ));
        }
        let mut done = turn("long", 100);
        done.status = "completed";
        done.completed_at_unix_ms = Some(100 + MAX_RECENT_REQUESTS as u64 + 10);
        let result = snapshot(&registry, done);
        assert_eq!(result.step_count, Some(MAX_RECENT_REQUESTS as u64 - 1));
        assert_eq!(result.average_output_tokens_per_second, Some(100.0));
        assert_eq!(result.runtime_totals.excluded_boundary_requests, 1);
        assert_eq!(result.coverage, "partial");
    }

    #[test]
    fn last_turn_active_boundary_request_does_not_contribute_live_time_or_speed() {
        let registry = ConversationStatsRegistry::default();
        registry.begin_request("own", "ambiguous", 200);
        registry.mark_request_upstream_started("own", "ambiguous", Instant::now());
        registry.update_request_speed(
            "own",
            "ambiguous",
            LiveSpeedSample {
                output_tokens: 300.0,
                generation_started_at: Instant::now() - Duration::from_secs(1),
                is_estimate: true,
            },
            201,
        );
        let result = snapshot(&registry, turn("new", 200));
        assert_eq!(result.in_flight, Some(0));
        assert_eq!(result.model_duration_ms, None);
        assert_eq!(result.average_output_tokens_per_second, None);
        assert!(!result.speed_is_estimate);
        assert_eq!(result.coverage, "partial");
    }
}
