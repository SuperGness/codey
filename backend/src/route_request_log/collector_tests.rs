use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tokio::sync::oneshot;

use super::super::test_support::sample_entry;
use super::super::{
    FirstByteSource, MAX_LOG_ERROR_BYTES, RequestBodySummary, RequestInputState, RequestProtocol,
    RequestStatus, RequestTokenUsage, RouteRequestLogQuery, RouteRequestLogStart,
    RouteRequestLogStats, SQLITE_FILE_NAME, UpstreamTransport, duration_millis, lock_unpoisoned,
    output_tokens_per_second, query_route_request_logs,
};
use super::{
    RouteRequestLogController, RouteRequestLogProbe, RouteRequestLogProducer,
    RouteRequestLogReconfigure, RouteRequestLogRuntime, elapsed_millis, merge_usage,
};
use crate::config::{RouteRequestLogBackend, RouteRequestLogConfig};

#[test]
fn attempt_diagnostics_are_bounded_and_never_copy_payloads() {
    use super::super::{AttemptOutcome, AttemptPhase, AttemptTrigger, MAX_RECORDED_ATTEMPTS};
    let probe = RouteRequestLogProbe::detached_test_probe();
    probe.mark_fallback("websocket_connect_failed");
    for _ in 0..MAX_RECORDED_ATTEMPTS + 5 {
        probe.begin_upstream_attempt(UpstreamTransport::HttpSse, AttemptTrigger::Initial);
        probe.record_attempt_headers(503);
    }
    probe.finish_upstream_attempt(AttemptOutcome::Failed, AttemptPhase::ResponseBody);
    let pending = lock_unpoisoned(&probe.shared.entry);
    assert_eq!(pending.upstream_attempts.len(), MAX_RECORDED_ATTEMPTS);
    assert_eq!(pending.attempts_omitted, 5);
    assert_eq!(
        pending.upstream_attempts[0].trigger,
        AttemptTrigger::TransportFallback
    );
    assert_eq!(
        pending.upstream_attempts.last().unwrap().outcome,
        AttemptOutcome::Rejected
    );
    let encoded = serde_json::to_string(&pending.upstream_attempts).unwrap();
    assert!(encoded.len() < 4_096);
    for forbidden in ["authorization", "headers", "input", "apiKey", "requestBody"] {
        assert!(!encoded.contains(forbidden));
    }
}

#[test]
fn attempts_distinguish_cancellation_timeout_and_failed_downstream_delivery() {
    use super::super::{AttemptOutcome, AttemptPhase, AttemptTrigger};
    for outcome in [AttemptOutcome::Cancelled, AttemptOutcome::TimedOut] {
        let probe = RouteRequestLogProbe::detached_test_probe();
        probe.begin_upstream_attempt(UpstreamTransport::WebSocket, AttemptTrigger::Initial);
        probe.finish_upstream_attempt(outcome, AttemptPhase::ResponseBody);
        let attempts = probe.attempts_for_test();
        assert_eq!(attempts[0].outcome, outcome);
        assert_eq!(attempts[0].status_code, None);
    }
    let probe = RouteRequestLogProbe::detached_test_probe();
    probe.begin_upstream_attempt(UpstreamTransport::HttpSse, AttemptTrigger::Initial);
    probe.observe_event(&serde_json::json!({"type":"response.completed","response":{"output":[{"text":"secret content"}]}}));
    probe.finish_upstream_attempt(AttemptOutcome::Failed, AttemptPhase::DownstreamDelivery);
    let attempts = probe.attempts_for_test();
    assert_eq!(attempts[0].outcome, AttemptOutcome::DeliveryFailed);
    assert_eq!(attempts[0].phase, AttemptPhase::DownstreamDelivery);
    assert!(
        !serde_json::to_string(&attempts)
            .unwrap()
            .contains("secret content")
    );
}

#[test]
fn cancellation_preserves_an_observed_upstream_failure() {
    use super::super::{AttemptOutcome, AttemptPhase, AttemptTrigger};
    let probe = RouteRequestLogProbe::detached_test_probe();
    probe.begin_upstream_attempt(UpstreamTransport::HttpSse, AttemptTrigger::Initial);
    probe.record_attempt_headers(200);
    probe.observe_event(
        &serde_json::json!({"type":"response.failed","response":{"status":"failed"}}),
    );
    probe.finish_upstream_attempt(AttemptOutcome::Cancelled, AttemptPhase::ResponseBody);
    let attempts = probe.attempts_for_test();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].outcome, AttemptOutcome::Failed);
    assert_eq!(attempts[0].phase, AttemptPhase::ResponseBody);
    assert!(attempts[0].duration_ms.is_some());
}

#[test]
fn statistics_only_probe_does_not_allocate_diagnostics() {
    use super::super::{AttemptTrigger, RouteDecision};
    let controller = RouteRequestLogController::with_root(PathBuf::new());
    let probe = controller
        .begin(|producer| {
            producer.begin(RouteRequestLogStart {
                request_id: "stats-only",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Http,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: Some("session"),
                codex_session_is_parent: false,
            })
        })
        .unwrap();
    assert!(!probe.records_diagnostics());
    probe.record_route_decision(RouteDecision::default());
    probe.begin_upstream_attempt(UpstreamTransport::Http, AttemptTrigger::Initial);
    let pending = lock_unpoisoned(&probe.shared.entry);
    assert!(pending.route_decision.is_none());
    assert_eq!(pending.upstream_attempts.capacity(), 0);
}

#[test]
fn live_speed_updates_before_finish_and_reconciles_reported_usage() {
    let controller = RouteRequestLogController::with_root(PathBuf::new());
    let registry = controller.session_stats();
    let now = Instant::now();
    let probe = controller
        .begin(|producer| {
            producer.begin(RouteRequestLogStart {
                request_id: "live",
                started_at: now - Duration::from_secs(6),
                request_protocol: RequestProtocol::Sse,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: Some("live-session"),
                codex_session_is_parent: false,
            })
        })
        .unwrap();
    probe
        .shared
        .upstream_started_at
        .set(now - Duration::from_secs(5))
        .unwrap();
    probe.shared.first_byte_micros.store(1, Ordering::Relaxed);
    probe
        .shared
        .router_pre_upstream_micros
        .store(1_000_001, Ordering::Relaxed);
    let waiting = registry.snapshot("live-session").unwrap();
    assert_eq!(waiting.speed_state, Some("waiting"));
    probe.observe_event_at(
        &serde_json::json!({"type":"response.created","response":{"usage":{"output_tokens":0}}}),
        now - Duration::from_secs(5),
    );
    assert_eq!(
        registry.snapshot("live-session").unwrap().speed_state,
        Some("waiting")
    );
    probe.observe_event_at(
        &serde_json::json!({"type":"response.output_text.delta","delta":"a".repeat(400)}),
        now - Duration::from_secs(4),
    );
    let live = registry.snapshot("live-session").unwrap();
    assert_eq!(live.in_flight, Some(1));
    assert_eq!(live.speed_scope, Some("current_request"));
    assert_eq!(live.speed_state, Some("streaming"));
    assert!(live.speed_is_estimate);
    assert!((live.output_tokens_per_second.unwrap() - 20.0).abs() < 0.1);
    assert_eq!(live.output_tokens, None);
    probe.observe_event_at(
        &serde_json::json!({"usage":{"output_tokens":120}}),
        now - Duration::from_secs(3),
    );
    assert!(!registry.snapshot("live-session").unwrap().speed_is_estimate);
    probe.observe_event_at(&serde_json::json!({"type":"response.function_call_arguments.delta","delta":"b".repeat(40)}), now - Duration::from_secs(2));
    let live = registry.snapshot("live-session").unwrap();
    assert!(live.speed_is_estimate);
    assert!((live.output_tokens_per_second.unwrap() - 26.0).abs() < 0.1);
    assert_eq!(
        lock_unpoisoned(&probe.shared.live_output).estimated_quarters,
        440
    );
    probe.observe_event_at(&serde_json::json!({"type":"response.completed","response":{"output":[{"text":"a".repeat(400)}],"usage":{"output_tokens":150}}}), now - Duration::from_secs(1));
    assert_eq!(
        lock_unpoisoned(&probe.shared.live_output).estimated_quarters,
        440
    );
    probe.finish_success();
    let finished = registry.snapshot("live-session").unwrap();
    assert_eq!(finished.in_flight, Some(0));
    assert_eq!(finished.speed_scope, Some("latest_request"));
    assert_eq!(finished.speed_state, None);
    assert!(!finished.speed_is_estimate);
    assert_eq!(finished.output_tokens, Some(150));
    probe.observe_event(&serde_json::json!({"type":"response.output_text.delta","delta":"late"}));
    assert_eq!(
        registry.snapshot("live-session").unwrap().in_flight,
        Some(0)
    );
}

#[test]
fn live_speed_throttles_without_losing_counts_and_handles_missing_timing() {
    let now = Instant::now();
    let mut progress = super::LiveOutputProgress::default();
    assert!(progress.observe(0, Some(0), now, None).is_none());
    let first = progress.observe(4, None, now, None).unwrap();
    assert_eq!(first.output_tokens, 1.0);
    assert_eq!(first.generation_started_at, now);
    assert!(first.is_estimate);
    assert!(
        progress
            .observe(40, None, now + Duration::from_millis(100), None)
            .is_none()
    );
    let next = progress
        .observe(40, None, now + Duration::from_millis(250), None)
        .unwrap();
    assert_eq!(next.output_tokens, 21.0);
    assert_eq!(next.generation_started_at, now);
    let reported = progress
        .observe(0, Some(30), now + Duration::from_millis(500), None)
        .unwrap();
    assert!(reported.is_estimate);
    assert_eq!(reported.output_tokens, 30.0);
    let more = progress
        .observe(40, Some(30), now + Duration::from_millis(750), None)
        .unwrap();
    assert_eq!(more.output_tokens, 40.0);
    assert!(more.is_estimate);
}

#[test]
fn last_turn_statistics_keep_multiple_completed_steps_with_logging_disabled() {
    let directory = tempfile::tempdir().unwrap();
    let controller = RouteRequestLogController::with_root(directory.path().to_path_buf());
    let registry = controller.session_stats();
    let now = Instant::now();
    for (id, output, generation) in [("a", 100, 1000), ("b", 600, 3000)] {
        let probe = controller
            .begin(|producer| {
                producer.begin(RouteRequestLogStart {
                    request_id: id,
                    started_at: now - Duration::from_millis(generation + 300),
                    request_protocol: RequestProtocol::Sse,
                    request_kind: "responses",
                    requested_model: "model",
                    reasoning_effort: None,
                    thinking_budget_tokens: None,
                    codex_session_id: Some("disabled-session"),
                    codex_session_is_parent: false,
                })
            })
            .unwrap();
        assert!(probe.shared.producer.sender.is_none());
        probe
            .shared
            .router_pre_upstream_micros
            .store(100_001, Ordering::Relaxed);
        probe
            .shared
            .first_byte_micros
            .store(200_001, Ordering::Relaxed);
        probe.observe_event(&serde_json::json!({"type":"response.completed","response":{"usage":{"output_tokens":output}}}));
        probe.finish_success();
    }
    // 第一次查询在两个请求结束后，摘要仍可按原生轮边界恢复。
    let mut turn = crate::conversation_stats::LastTurnStatsSnapshot::default();
    turn.id = "turn".into();
    turn.started_at_unix_ms = Some(super::unix_timestamp_ms_at(now - Duration::from_secs(10)));
    turn.status = "running";
    let result = registry
        .snapshot_with_turn("disabled-session", Some(turn))
        .unwrap()
        .last_turn
        .unwrap();
    assert_eq!(result.step_count, Some(2));
    assert!((result.average_output_tokens_per_second.unwrap() - 175.0).abs() < 1.0);
    assert_eq!(result.coverage, "runtime");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn live_speed_ignores_parent_and_compaction_and_cancelled_requests() {
    let controller = RouteRequestLogController::with_root(PathBuf::new());
    let registry = controller.session_stats();
    for (id, kind, parent) in [
        ("own", "responses", false),
        ("parent", "responses", true),
        ("compact", "responses_compact", false),
    ] {
        let probe = controller
            .begin(|producer| {
                producer.begin(RouteRequestLogStart {
                    request_id: id,
                    started_at: Instant::now(),
                    request_protocol: RequestProtocol::WebSocket,
                    request_kind: kind,
                    requested_model: "model",
                    reasoning_effort: None,
                    thinking_budget_tokens: None,
                    codex_session_id: Some(id),
                    codex_session_is_parent: parent,
                })
            })
            .unwrap();
        probe.observe_event(
            &serde_json::json!({"type":"response.reasoning_summary_text.delta","delta":"thinking"}),
        );
        if id == "own" {
            let live = registry.snapshot(id).unwrap();
            assert_eq!(live.in_flight, Some(1));
            assert_eq!(live.speed_state, Some("streaming"));
            assert_eq!(live.output_tokens_per_second, None);
        } else {
            assert!(registry.snapshot(id).is_none());
        }
        probe.finish_cancelled();
        let snapshot = registry.snapshot(id).unwrap();
        assert_eq!(snapshot.in_flight, Some(0));
        assert_eq!(snapshot.speed_state, None);
        assert_eq!(snapshot.output_tokens_per_second, None);
    }
}

#[test]
fn live_speed_and_usage_survive_log_sampling_without_writing_unsampled_requests() {
    let (sender, receiver) = mpsc::sync_channel(2);
    let registry = Arc::new(crate::conversation_stats::ConversationStatsRegistry::default());
    let stats = Arc::new(RouteRequestLogStats::default());
    let producer = RouteRequestLogProducer {
        sender: Some(sender),
        accepting: Arc::new(AtomicBool::new(true)),
        submitting: Arc::new(AtomicU64::new(0)),
        sample_rate_per_million: 1,
        sample_sequence: Arc::new(AtomicU64::new(0)),
        stats: Arc::clone(&stats),
        session_stats: Some(Arc::clone(&registry)),
    };
    let now = Instant::now();
    let probe = producer
        .begin(RouteRequestLogStart {
            request_id: "unsampled",
            started_at: now - Duration::from_secs(3),
            request_protocol: RequestProtocol::Sse,
            request_kind: "responses",
            requested_model: "model",
            reasoning_effort: None,
            thinking_budget_tokens: None,
            codex_session_id: Some("unsampled-session"),
            codex_session_is_parent: false,
        })
        .expect("unsampled session still receives a statistics probe");
    assert_eq!(stats.sampled_out.load(Ordering::Relaxed), 1);
    assert!(probe.shared.producer.sender.is_none());
    probe.observe_event_at(
        &serde_json::json!({"type":"response.output_text.delta","delta":"a".repeat(400)}),
        now - Duration::from_secs(1),
    );
    let live = registry.snapshot("unsampled-session").unwrap();
    assert_eq!(live.in_flight, Some(1));
    assert!(live.output_tokens_per_second.unwrap() > 90.0);
    probe.observe_event(&serde_json::json!({"type":"response.completed","response":{"usage":{"output_tokens":150}}}));
    probe.finish_success();
    let complete = registry.snapshot("unsampled-session").unwrap();
    assert_eq!(complete.in_flight, Some(0));
    assert_eq!(complete.output_tokens, Some(150));
    assert_eq!(complete.requests, Some(1));
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(stats.accepted.load(Ordering::Relaxed), 0);
}

#[test]
fn billing_tiers_preserve_request_and_actual_fallback() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    assert_eq!(lock_unpoisoned(&probe.shared.entry).service_tier, None);
    probe.set_requested_service_tier(Some("priority"));
    probe.observe_event(
        &serde_json::json!({"type":"response.created","response":{"service_tier":"fast"}}),
    );
    assert_eq!(
        lock_unpoisoned(&probe.shared.entry).service_tier.as_deref(),
        Some("fast")
    );
    probe.observe_response(
        200,
        &serde_json::json!({"service_tier":"default", "usage": {
            "input_tokens": 2000, "input_tokens_details": {"cache_write_tokens": 1500},
            "cache_creation_input_tokens": 999
        }}),
    );
    let entry = lock_unpoisoned(&probe.shared.entry);
    assert_eq!(entry.requested_service_tier.as_deref(), Some("priority"));
    assert_eq!(entry.service_tier.as_deref(), Some("default"));
    assert_eq!(entry.token_usage.cache_creation_input_tokens, Some(1500));
}

#[test]
fn request_body_summary_classifies_input_shape() {
    let absent =
        RequestBodySummary::from_responses_body(&serde_json::json!({"model": "m"}), Some(64));
    assert_eq!(absent.input_state, RequestInputState::Absent);
    assert_eq!(absent.input_items, 0);
    assert!(!absent.has_previous_response_id);
    let null = RequestBodySummary::from_responses_body(&serde_json::json!({"input": null}), None);
    assert_eq!(null.input_state, RequestInputState::Null);
    assert_eq!(null.bytes, None);
    // 空数组正是上游报错 Input items array must not be empty 的形态，
    // 必须能和正常输入区分开。
    let empty =
        RequestBodySummary::from_responses_body(&serde_json::json!({"input": []}), Some(128));
    assert_eq!(empty.input_state, RequestInputState::Array);
    assert_eq!(empty.input_items, 0);
    let items = RequestBodySummary::from_responses_body(
        &serde_json::json!({
            "input": [{"role": "user"}, {"role": "user"}],
            "previous_response_id": "resp_1"
        }),
        Some(256),
    );
    assert_eq!(items.input_state, RequestInputState::Array);
    assert_eq!(items.input_items, 2);
    assert!(items.has_previous_response_id);
    assert_eq!(items.bytes, Some(256));
}

#[test]
fn raw_upstream_model_outranks_the_relayed_downstream_model() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    probe.observe_event(&serde_json::json!({
        "type": "response.completed",
        "response": {"model": "provider-model", "status": "completed"},
    }));
    assert_eq!(
        probe.upstream_response_model_for_test().as_deref(),
        Some("provider-model")
    );
    probe.observe_upstream_response_model("deepseek/deepseek-v4.1-flash");
    assert_eq!(
        probe.upstream_response_model_for_test().as_deref(),
        Some("deepseek/deepseek-v4.1-flash")
    );
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
fn optional_timing_fields_preserve_ttft_wire_compatibility() {
    let mut entry = sample_entry("timing");
    entry.router_pre_upstream_ms = None;
    entry.upstream_first_byte_ms = None;
    entry.downstream_first_content_ms = None;

    let value = serde_json::to_value(&entry).unwrap();
    assert_eq!(value["ttftMs"], 12);
    assert!(value.get("routerPreUpstreamMs").is_none());
    assert!(value.get("upstreamFirstByteMs").is_none());
    assert!(value.get("downstreamFirstContentMs").is_none());
}

#[test]
fn output_speed_requires_the_shared_thresholds_and_a_generation_window() {
    assert_eq!(
        output_tokens_per_second(Some(100), 1_000, Some(50), Some(150)),
        Some(125.0)
    );
    assert_eq!(
        output_tokens_per_second(Some(100), 1_000, None, Some(200)),
        None
    );
    assert_eq!(
        output_tokens_per_second(Some(100), 1_000, Some(0), None),
        None
    );
    assert_eq!(
        output_tokens_per_second(Some(100), 200, Some(50), Some(150)),
        None
    );
    assert_eq!(
        output_tokens_per_second(Some(100), 100, Some(50), Some(150)),
        None
    );
    assert_eq!(
        output_tokens_per_second(Some(100), u64::MAX, Some(u64::MAX), Some(1)),
        None
    );
    // 输出或生成窗口任意一项低于门槛都不计速度，避免短回复失真。
    assert_eq!(
        output_tokens_per_second(Some(99), 10_000, Some(0), Some(0)),
        None
    );
    assert_eq!(
        output_tokens_per_second(Some(1_000), 299, Some(50), Some(150)),
        None
    );
    assert_eq!(
        output_tokens_per_second(Some(1_000), 300, Some(50), Some(150)),
        Some(10_000.0)
    );
    assert_eq!(
        output_tokens_per_second(Some(0), 1_000, Some(50), Some(150)),
        None
    );
    assert_eq!(
        output_tokens_per_second(None, 1_000, Some(50), Some(150)),
        None
    );
}

#[test]
fn disabled_runtime_creates_no_queue_or_thread() {
    let config = RouteRequestLogConfig::default();
    let directory = tempfile::tempdir().unwrap();
    assert!(
        RouteRequestLogRuntime::start_at(&config, directory.path().to_path_buf(), Arc::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn disabled_controller_evaluates_start_but_skips_sessionless_requests() {
    let controller = RouteRequestLogController::with_root(PathBuf::new());
    let evaluated = AtomicBool::new(false);
    // 落盘关闭时仍走会话统计 producer：闭包会求值，但无会话标识的请求
    // 依旧短路，不产生 probe 开销。
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
    assert!(evaluated.load(Ordering::Relaxed));

    let session_probe = controller.begin(|producer| {
        producer.begin(RouteRequestLogStart {
            request_id: "stats-only",
            started_at: Instant::now(),
            request_protocol: RequestProtocol::Http,
            request_kind: "responses",
            requested_model: "model",
            reasoning_effort: None,
            thinking_budget_tokens: None,
            codex_session_id: Some("thread-stats"),
            codex_session_is_parent: false,
        })
    });
    assert!(session_probe.is_some());
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
    // 落盘停掉后 begin 仍落到纯统计 producer：闭包会执行，但无会话标识的
    // 请求依旧拿不到 probe，也不会写进 SQLite。
    assert!(
        controller
            .begin(|producer| producer.begin(RouteRequestLogStart {
                request_id: "after-disable",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Http,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: None,
                codex_session_is_parent: false,
            }))
            .is_none()
    );
    let connection = Connection::open(directory.path().join(SQLITE_FILE_NAME)).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM route_request_logs", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn controller_clear_restarts_enabled_writer_and_accepts_new_entries() {
    let directory = tempfile::tempdir().unwrap();
    let controller = RouteRequestLogController::with_root(directory.path().to_path_buf());
    let config = RouteRequestLogConfig {
        enabled: true,
        backend: RouteRequestLogBackend::Sqlite,
        // Allow slow CI disk I/O; shutdown deadlines have separate coverage.
        shutdown_flush_timeout_ms: 10_000,
        ..RouteRequestLogConfig::default()
    };
    controller.reconfigure(&config).await.unwrap();
    controller
        .active
        .load_full()
        .unwrap()
        .submit(sample_entry("before-clear"));

    let cleared = controller.clear().await;

    assert_eq!(cleared.status, "ok", "{cleared:#?}");
    assert!(cleared.recording_active);
    assert!(cleared.recording_restarted);
    let empty = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(empty.total, 0);

    controller
        .active
        .load_full()
        .unwrap()
        .submit(sample_entry("after-clear"));
    controller.stop().await;
    let after = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(after.total, 1);
    assert_eq!(after.items[0].request_id, "after-clear");
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
    // sink 启动失败不发布落盘 producer；会话统计 producer 仍兜底，但同样
    // 只接受带会话标识的请求。
    assert!(
        controller
            .begin(|producer| producer.begin(RouteRequestLogStart {
                request_id: "unpublished",
                started_at: Instant::now(),
                request_protocol: RequestProtocol::Http,
                request_kind: "responses",
                requested_model: "model",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: None,
                codex_session_is_parent: false,
            }))
            .is_none()
    );
}

#[test]
fn full_queue_drops_without_backpressure_and_counts_the_drop() {
    let (sender, _receiver) = mpsc::sync_channel(1);
    let stats = Arc::new(RouteRequestLogStats::default());
    let producer = RouteRequestLogProducer {
        sender: Some(sender),
        accepting: Arc::new(AtomicBool::new(true)),
        submitting: Arc::new(AtomicU64::new(0)),
        sample_rate_per_million: 1_000_000,
        sample_sequence: Arc::new(AtomicU64::new(0)),
        stats: Arc::clone(&stats),
        session_stats: None,
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
        sender: Some(sender),
        accepting: Arc::new(AtomicBool::new(true)),
        submitting: Arc::new(AtomicU64::new(0)),
        sample_rate_per_million: 1_000_000,
        sample_sequence: Arc::new(AtomicU64::new(0)),
        stats,
        session_stats: None,
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
        Some("account-a"),
        "alias/model",
        "model",
        "api.example.com",
        "OpenAI Responses",
        "Responses passthrough",
        false,
    );
    use super::super::{AttemptOutcome, AttemptTrigger, RouteDecision, RouteDecisionReason};
    probe.record_route_decision(RouteDecision {
        settings_revision: 42,
        reason: RouteDecisionReason::ExplicitAlias,
        candidate_count: 1,
        ..Default::default()
    });
    probe.begin_upstream_attempt(UpstreamTransport::HttpSse, AttemptTrigger::Initial);
    probe.record_attempt_headers(400);
    probe.begin_upstream_attempt(UpstreamTransport::HttpSse, AttemptTrigger::ReasoningRepair);
    probe.record_attempt_headers(200);
    probe.mark_first_upstream_data(FirstByteSource::UpstreamHttpBody);
    probe.mark_first_downstream_content();
    let original_error = format!("{}末尾", "错".repeat(MAX_LOG_ERROR_BYTES / 3));
    probe.mark_upstream_error_summary(&original_error);
    probe.mark_upstream_error_summary("later classification must not replace the original");
    probe.observe_event(&serde_json::json!({
        "type":"response.completed",
        "response":{"usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}
    }));
    probe.finish_success();
    probe.finish_cancelled();

    let queued = receiver.try_recv().unwrap();
    assert_eq!(queued.entry.request_id, "request-one");
    assert_eq!(queued.entry.status, RequestStatus::Succeeded);
    assert_eq!(queued.entry.route_decision.unwrap().settings_revision, 42);
    assert_eq!(queued.entry.upstream_attempts.len(), 2);
    assert_eq!(
        queued.entry.upstream_attempts[0].outcome,
        AttemptOutcome::Rejected
    );
    assert_eq!(queued.entry.upstream_attempts[0].status_code, Some(400));
    assert_eq!(
        queued.entry.upstream_attempts[1].outcome,
        AttemptOutcome::Succeeded
    );
    assert_eq!(
        queued.entry.upstream_attempts[1].trigger,
        AttemptTrigger::ReasoningRepair
    );
    assert!(queued.entry.ttft_ms.is_some());
    assert!(queued.entry.router_pre_upstream_ms.is_some());
    assert_eq!(queued.entry.upstream_first_byte_ms, queued.entry.ttft_ms);
    assert!(queued.entry.downstream_first_content_ms.is_some());
    assert_eq!(queued.entry.token_usage.total_tokens, Some(15));
    assert_eq!(queued.entry.codex_session_id.as_deref(), Some("thread-one"));
    assert!(!queued.entry.codex_session_is_parent);
    assert_eq!(
        queued.entry.official_account_id.as_deref(),
        Some("account-a")
    );
    assert_eq!(
        queued.entry.upstream_error_summary.as_deref(),
        Some(
            format!(
                "{}\n[错误内容达到记录上限，内容可能不完整]",
                "错".repeat(MAX_LOG_ERROR_BYTES / 3),
            )
            .as_str()
        )
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn deferred_finish_waits_for_observer_and_still_submits_exactly_once() {
    let (sender, receiver) = mpsc::sync_channel(2);
    let producer = RouteRequestLogProducer {
        sender: Some(sender),
        accepting: Arc::new(AtomicBool::new(true)),
        submitting: Arc::new(AtomicU64::new(0)),
        sample_rate_per_million: 1_000_000,
        sample_sequence: Arc::new(AtomicU64::new(0)),
        stats: Arc::new(RouteRequestLogStats::default()),
        session_stats: None,
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

    let written_at = Instant::now();
    probe.finish_success();
    let completed_duration_ms = elapsed_millis(probe.shared.started_at);
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    std::thread::sleep(Duration::from_millis(20));
    probe.mark_first_downstream_content_at(written_at);
    probe.observe_event(&serde_json::json!({
        "type":"response.completed",
        "response":{"usage":{"input_tokens":9,"output_tokens":4,"total_tokens":13}}
    }));
    drop(observer);

    let entry = receiver.try_recv().unwrap().entry;
    assert!(entry.total_duration_ms <= completed_duration_ms);
    assert_eq!(
        entry.downstream_first_content_ms,
        Some(duration_millis(
            written_at.duration_since(probe.shared.started_at)
        ))
    );
    assert_eq!(entry.status, RequestStatus::Succeeded);
    assert_eq!(entry.token_usage.total_tokens, Some(13));
    probe.finish_cancelled();
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
}

#[test]
fn synthetic_response_created_does_not_set_ttft() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let producer = RouteRequestLogProducer {
        sender: Some(sender),
        accepting: Arc::new(AtomicBool::new(true)),
        submitting: Arc::new(AtomicU64::new(0)),
        sample_rate_per_million: 1_000_000,
        sample_sequence: Arc::new(AtomicU64::new(0)),
        stats: Arc::new(RouteRequestLogStats::default()),
        session_stats: None,
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
        sender: Some(sender),
        accepting: Arc::new(AtomicBool::new(true)),
        submitting: Arc::new(AtomicU64::new(0)),
        sample_rate_per_million: 1_000_000,
        sample_sequence: Arc::new(AtomicU64::new(0)),
        stats: Arc::new(RouteRequestLogStats::default()),
        session_stats: None,
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
    probe.mark_upstream_send(UpstreamTransport::Http);
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
        sender: Some(sender),
        accepting: Arc::new(AtomicBool::new(true)),
        submitting: Arc::new(AtomicU64::new(0)),
        sample_rate_per_million: 1_000_000,
        sample_sequence: Arc::new(AtomicU64::new(0)),
        stats: Arc::new(RouteRequestLogStats::default()),
        session_stats: None,
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
    probe.mark_usage_unavailable("observer_queue_full");
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
