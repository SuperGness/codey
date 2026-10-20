use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::super::test_support::{queued, sample_entry};
use super::super::{
    RequestInputState, RouteRequestLogQuery, RouteRequestLogStats, SQLITE_FILE_NAME,
    output_tokens_per_second, query_route_request_log_stats, query_route_request_logs,
    unix_timestamp_ms,
};
use super::{BatchSink, NdjsonSink, SqliteSink, flush_batch, prune_sqlite_logs, rotated_path};
use crate::config::RouteRequestLogBackend;

#[test]
fn route_diagnostics_migrate_old_logs_and_roundtrip_without_losing_failed_attempts() {
    use super::super::{
        AttemptOutcome, AttemptPhase, AttemptTrigger, RouteDecision, RouteDecisionReason,
        UpstreamAttemptLog, UpstreamTransport,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut legacy = sample_entry("legacy");
    legacy.timestamp_unix_ms = unix_timestamp_ms();
    sink.write_batch(&[queued(legacy)]).unwrap();
    sink.connection.execute_batch("ALTER TABLE route_request_logs DROP COLUMN route_decision; ALTER TABLE route_request_logs DROP COLUMN upstream_attempts; ALTER TABLE route_request_logs DROP COLUMN attempts_omitted;").unwrap();
    drop(sink);
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert!(page.items[0].route_decision.is_none());
    assert!(page.items[0].upstream_attempts.is_empty());
    assert_eq!(page.items[0].attempts_omitted, 0);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut entry = sample_entry("diagnostics");
    entry.timestamp_unix_ms = unix_timestamp_ms();
    entry.route_decision = Some(RouteDecision {
        settings_revision: 91,
        reason: RouteDecisionReason::ExplicitAlias,
        candidate_count: 1,
        ..Default::default()
    });
    entry.upstream_attempts = vec![
        UpstreamAttemptLog {
            sequence: 1,
            transport: UpstreamTransport::HttpSse,
            trigger: AttemptTrigger::Initial,
            started_ms: 5,
            duration_ms: Some(8),
            header_ms: Some(4),
            status_code: Some(429),
            outcome: AttemptOutcome::Rejected,
            phase: AttemptPhase::ResponseHeaders,
        },
        UpstreamAttemptLog {
            sequence: 2,
            transport: UpstreamTransport::HttpSse,
            trigger: AttemptTrigger::PluginRetry,
            started_ms: 14,
            duration_ms: Some(30),
            header_ms: Some(4),
            status_code: Some(200),
            outcome: AttemptOutcome::Succeeded,
            phase: AttemptPhase::Complete,
        },
    ];
    let expected = entry.clone();
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["routeDecision"]["settingsRevision"], 91);
    assert_eq!(json["upstreamAttempts"][0]["statusCode"], 429);
    sink.write_batch(&[queued(entry)]).unwrap();
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    let item = page
        .items
        .iter()
        .find(|item| item.request_id == "diagnostics")
        .unwrap();
    assert_eq!(item.route_decision, expected.route_decision);
    assert_eq!(item.upstream_attempts, expected.upstream_attempts);
    let legacy = page
        .items
        .iter()
        .find(|item| item.request_id == "legacy")
        .unwrap();
    assert!(legacy.route_decision.is_none());
    assert!(legacy.upstream_attempts.is_empty());
}

#[test]
fn billing_tiers_migrate_history_and_roundtrip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut old = sample_entry("old");
    old.timestamp_unix_ms = unix_timestamp_ms();
    sink.write_batch(&[queued(old)]).unwrap();
    sink.connection.execute_batch("ALTER TABLE route_request_logs DROP COLUMN requested_service_tier; ALTER TABLE route_request_logs DROP COLUMN service_tier;").unwrap();
    drop(sink);
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.items[0].service_tier, None);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut entry = sample_entry("new");
    entry.timestamp_unix_ms = unix_timestamp_ms();
    entry.requested_service_tier = Some("flex".into());
    entry.service_tier = Some("default".into());
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["requestedServiceTier"], "flex");
    assert_eq!(json["serviceTier"], "default");
    sink.write_batch(&[queued(entry)]).unwrap();
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    let old = page
        .items
        .iter()
        .find(|item| item.request_id == "old")
        .unwrap();
    assert_eq!(
        (
            old.requested_service_tier.as_deref(),
            old.service_tier.as_deref()
        ),
        (None, None)
    );
    let new = page
        .items
        .iter()
        .find(|item| item.request_id == "new")
        .unwrap();
    assert_eq!(
        (
            new.requested_service_tier.as_deref(),
            new.service_tier.as_deref()
        ),
        (Some("flex"), Some("default"))
    );
}

#[test]
fn response_headers_column_migrates_history_and_roundtrips() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut legacy = sample_entry("legacy");
    legacy.timestamp_unix_ms = unix_timestamp_ms();
    sink.write_batch(&[queued(legacy)]).unwrap();
    sink.connection
        .execute_batch("ALTER TABLE route_request_logs DROP COLUMN upstream_response_headers;")
        .unwrap();
    drop(sink);
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    // 旧库缺少响应头列时查询照常返回，该字段按未记录处理。
    assert_eq!(page.items[0].upstream_response_headers, None);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut entry = sample_entry("response-headers");
    entry.timestamp_unix_ms = unix_timestamp_ms();
    sink.write_batch(&[queued(entry)]).unwrap();
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    let item = page
        .items
        .iter()
        .find(|item| item.request_id == "response-headers")
        .unwrap();
    assert_eq!(
        item.upstream_response_headers.as_deref(),
        Some("x-codex-turn-state: routed\nset-cookie: [REDACTED]")
    );
}

#[test]
fn upstream_response_model_column_migrates_history_and_roundtrips() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut legacy = sample_entry("legacy");
    legacy.timestamp_unix_ms = unix_timestamp_ms();
    sink.write_batch(&[queued(legacy)]).unwrap();
    sink.connection
        .execute_batch("ALTER TABLE route_request_logs DROP COLUMN upstream_response_model;")
        .unwrap();
    drop(sink);
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    // 旧库缺少该列时查询照常返回，字段按上游未回报处理。
    assert_eq!(page.items[0].upstream_response_model, None);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut entry = sample_entry("upstream-model");
    entry.timestamp_unix_ms = unix_timestamp_ms();
    entry.upstream_response_model = Some("deepseek/deepseek-v4.1-flash".into());
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        json["upstreamResponseModel"],
        "deepseek/deepseek-v4.1-flash"
    );
    sink.write_batch(&[queued(entry)]).unwrap();
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    let item = page
        .items
        .iter()
        .find(|item| item.request_id == "upstream-model")
        .unwrap();
    assert_eq!(
        item.upstream_response_model.as_deref(),
        Some("deepseek/deepseek-v4.1-flash")
    );
}

#[test]
fn request_shape_columns_migrate_history_and_roundtrip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut legacy = sample_entry("legacy");
    legacy.timestamp_unix_ms = unix_timestamp_ms();
    sink.write_batch(&[queued(legacy)]).unwrap();
    sink.connection
        .execute_batch(
            "ALTER TABLE route_request_logs DROP COLUMN request_input_state;
             ALTER TABLE route_request_logs DROP COLUMN request_input_items;
             ALTER TABLE route_request_logs DROP COLUMN request_has_previous_response_id;
             ALTER TABLE route_request_logs DROP COLUMN request_bytes;
             ALTER TABLE route_request_logs DROP COLUMN upstream_input_state;
             ALTER TABLE route_request_logs DROP COLUMN upstream_input_items;
             ALTER TABLE route_request_logs DROP COLUMN upstream_has_previous_response_id;
             ALTER TABLE route_request_logs DROP COLUMN upstream_bytes;",
        )
        .unwrap();
    drop(sink);
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.items[0].request_input_state, None);
    assert_eq!(page.items[0].upstream_input_items, None);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut entry = sample_entry("shape");
    entry.timestamp_unix_ms = unix_timestamp_ms();
    entry.request_input_state = Some(RequestInputState::Array);
    entry.request_input_items = Some(0);
    entry.request_bytes = Some(512);
    entry.upstream_input_state = Some(RequestInputState::Absent);
    entry.upstream_input_items = Some(0);
    entry.upstream_bytes = Some(1024);
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["requestInputState"], "array");
    assert_eq!(json["requestInputItems"], 0);
    assert_eq!(json["upstreamInputState"], "absent");
    sink.write_batch(&[queued(entry)]).unwrap();
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    let legacy = page
        .items
        .iter()
        .find(|item| item.request_id == "legacy")
        .unwrap();
    assert_eq!(legacy.request_input_state, None);
    assert_eq!(legacy.request_bytes, None);
    let shape = page
        .items
        .iter()
        .find(|item| item.request_id == "shape")
        .unwrap();
    assert_eq!(shape.request_input_state.as_deref(), Some("array"));
    assert_eq!(shape.request_input_items, Some(0));
    assert_eq!(shape.request_bytes, Some(512));
    assert_eq!(shape.upstream_input_state.as_deref(), Some("absent"));
    assert_eq!(shape.upstream_bytes, Some(1024));
}

#[test]
fn official_account_id_migrates_history_and_filters_one_account() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut legacy = sample_entry("legacy");
    legacy.timestamp_unix_ms = unix_timestamp_ms();
    sink.write_batch(&[queued(legacy)]).unwrap();
    sink.connection
        .execute_batch("ALTER TABLE route_request_logs DROP COLUMN official_account_id;")
        .unwrap();
    drop(sink);

    // 旧库还没有账号列时查询照常返回，账号筛选被忽略而不是让查询失败。
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].official_account_id.is_none());
    let filtered = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            official_account_id: Some("account-b".into()),
            ..RouteRequestLogQuery::default()
        },
    )
    .unwrap();
    assert_eq!(filtered.total, 1);

    let mut sink = SqliteSink::open(&path, 30).unwrap();
    for (request_id, account_id) in [("first", "account-a"), ("second", "account-b")] {
        let mut entry = sample_entry(request_id);
        entry.timestamp_unix_ms = unix_timestamp_ms();
        entry.official_account_id = Some(account_id.to_string());
        sink.write_batch(&[queued(entry)]).unwrap();
    }
    let json = serde_json::to_value({
        let mut named = sample_entry("named");
        named.official_account_id = Some("account-b".into());
        named
    })
    .unwrap();
    assert_eq!(json["officialAccountId"], "account-b");

    let query_for = |account_id: &str| {
        query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                official_account_id: Some(account_id.to_string()),
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap()
    };
    let second = query_for("account-b");
    assert_eq!(second.total, 1);
    assert_eq!(second.items[0].request_id, "second");
    assert_eq!(
        second.items[0].official_account_id.as_deref(),
        Some("account-b")
    );
    assert_eq!(query_for("account-a").items[0].request_id, "first");

    let searched = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            search: Some("account-a".into()),
            ..RouteRequestLogQuery::default()
        },
    )
    .unwrap();
    assert_eq!(searched.total, 1);
    assert_eq!(searched.items[0].request_id, "first");
}

#[test]
fn sqlite_output_speed_uses_upstream_timing_and_rejects_invalid_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut buffered = sample_entry("buffered");
    buffered.timestamp_unix_ms = 100;
    buffered.total_duration_ms = 112_480;
    buffered.router_pre_upstream_ms = Some(23);
    buffered.upstream_first_byte_ms = Some(1_992);
    buffered.downstream_first_content_ms = Some(112_374);
    buffered.token_usage.output_tokens = Some(18_728);
    buffered.token_usage.reasoning_output_tokens = Some(1_000);
    // 旧的落盘速度不参与查询，历史记录也按当前计时口径重新计算。
    buffered.output_tokens_per_second = Some(176_679.0);
    let expected = output_tokens_per_second(Some(18_728), 112_480, Some(23), Some(1_992));
    assert_eq!(expected, Some(18_728_000.0 / 110_465.0));
    let mut no_downstream = buffered.clone();
    no_downstream.request_id = "no-downstream".into();
    no_downstream.downstream_first_content_ms = None;
    no_downstream.ttft_ms = Some(112_479);
    sink.write_batch(&[queued(buffered.clone()), queued(no_downstream)])
        .unwrap();

    // SQLite 历史记录可含负数；分步减法也应拒绝会使加法溢出的计时。
    let invalid = [
        ("missing-router", 112_480, None, Some(1_992)),
        ("missing-byte", 112_480, Some(23), None),
        ("negative-total", -1, Some(23), Some(1_992)),
        ("negative-router", 112_480, Some(-1), Some(1_992)),
        ("negative-byte", 112_480, Some(23), Some(-1)),
        ("inverted-router", 100, Some(101), Some(0)),
        ("inverted-byte", 100, Some(50), Some(51)),
        ("short-window", 299, Some(50), Some(150)),
        ("overflow", i64::MAX, Some(i64::MAX), Some(i64::MAX)),
    ];
    for (request_id, total, router, first_byte) in invalid {
        let mut entry = buffered.clone();
        entry.request_id = request_id.into();
        sink.write_batch(&[queued(entry)]).unwrap();
        sink.connection
            .execute(
                "UPDATE route_request_logs SET total_duration_ms = ?1,
                    router_pre_upstream_ms = ?2, upstream_first_byte_ms = ?3
                    WHERE request_id = ?4",
                rusqlite::params![total, router, first_byte, request_id],
            )
            .unwrap();
    }
    let query = RouteRequestLogQuery {
        from_unix_ms: Some(100),
        to_unix_ms: Some(200),
        group_by: Some("provider".into()),
        ..Default::default()
    };
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        query.clone(),
    )
    .unwrap();
    assert_eq!(page.items.len(), invalid.len() + 2);
    for entry in page.items {
        let valid = matches!(entry.request_id.as_str(), "buffered" | "no-downstream");
        assert_eq!(
            entry.output_tokens_per_second,
            if valid { expected } else { None },
            "{}",
            entry.request_id
        );
    }
    let stats =
        query_route_request_log_stats(directory.path(), RouteRequestLogBackend::Sqlite, query)
            .unwrap();
    assert_eq!(stats.summary.avg_output_tokens_per_second, expected);
    assert_eq!(stats.groups.len(), 1);
    assert_eq!(
        stats.groups[0].summary.avg_output_tokens_per_second,
        expected
    );
    assert_eq!(stats.trend.len(), 1);
    assert_eq!(stats.trend[0].avg_output_tokens_per_second, expected);
}

#[test]
fn sqlite_retry_keeps_the_batch_and_retention_deletes_bounded_chunks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let sqlite = SqliteSink::open(&path, 30).unwrap();
    sqlite.connection.busy_timeout(Duration::ZERO).unwrap();
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let stats = Arc::new(RouteRequestLogStats::default());
    let observed = Arc::clone(&stats);
    let release = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while observed.write_failures.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        blocker.execute_batch("COMMIT").unwrap();
    });
    let mut sink = BatchSink::Sqlite(sqlite);
    let mut batch = vec![queued(sample_entry("retry"))];
    assert!(flush_batch(&mut sink, &mut batch, &stats, &mut 0));
    release.join().unwrap();
    assert!(stats.snapshot().write_failures >= 1);
    assert_eq!(stats.snapshot().entries_written, 1);
    assert_eq!(stats.snapshot().write_dropped, 0);
    let BatchSink::Sqlite(sqlite) = &mut sink else {
        unreachable!()
    };
    sqlite
        .write_batch(
            &(0..1_500)
                .map(|index| queued(sample_entry(&format!("old-{index}"))))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(prune_sqlite_logs(&sqlite.connection, 0).unwrap(), 1_000);
    assert_eq!(prune_sqlite_logs(&sqlite.connection, 0).unwrap(), 501);
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
    assert!(contents.contains("\"routerPreUpstreamMs\":4"));
    assert!(contents.contains("\"upstreamFirstByteMs\":12"));
    assert!(contents.contains("\"downstreamFirstContentMs\":14"));
    assert!(!contents.contains("retryCount"));
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
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
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

    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            page_size: 10,
            ..RouteRequestLogQuery::default()
        },
    )
    .unwrap();
    assert_eq!(page.items[0].ttft_ms, Some(12));
    assert_eq!(page.items[0].router_pre_upstream_ms, Some(4));
    assert_eq!(page.items[0].upstream_first_byte_ms, Some(12));
    assert_eq!(page.items[0].downstream_first_content_ms, Some(14));
    assert_eq!(
        page.items[0].upstream_request_headers.as_deref(),
        Some("authorization: [REDACTED]\nx-test: value")
    );
    assert_eq!(
        page.items[0].upstream_response_headers.as_deref(),
        Some("x-codex-turn-state: routed\nset-cookie: [REDACTED]")
    );
}

#[test]
fn sqlite_concurrency_wait_is_independent_from_log_writer_queue_delay() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut entry = sample_entry("queued-official");
    entry.concurrency_wait_ms = Some(1_234);
    entry.queue_delay_ms = 7;
    sink.write_batch(&[queued(entry)]).unwrap();
    sink.finish().unwrap();
    drop(sink);
    let page = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.items[0].concurrency_wait_ms, Some(1_234));
    assert_eq!(page.items[0].queue_delay_ms, 7);
}

#[cfg(unix)]
#[test]
fn sqlite_sink_uses_private_file_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("requests.sqlite3");
    let sink = SqliteSink::open(&path, 30).unwrap();
    drop(sink);
    let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn sqlite_sink_prunes_expired_rows_during_normal_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("requests.sqlite3");
    let mut sink = SqliteSink::open(&path, 1).unwrap();
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
