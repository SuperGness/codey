use std::fs;

use rusqlite::{Connection, params_from_iter};

use super::super::test_support::{queued, sample_entry};
use super::super::writer::SqliteSink;
use super::super::{
    NDJSON_FILE_NAME, RequestProtocol, RequestStatus, RequestTokenUsage, RouteRequestLogCursor,
    RouteRequestLogModelQuery, RouteRequestLogQuery, RouteRequestLogQueryItem, SQLITE_FILE_NAME,
    UpstreamTransport, unix_timestamp_ms,
};
use super::{
    DAY_MS, MAX_QUERY_PAGE, MAX_QUERY_PAGE_SIZE, MAX_QUERY_SEARCH_BYTES,
    clear_route_request_log_files, latest_session_recorded_speed, query_route_request_log_models,
    query_route_request_log_stats, query_route_request_logs, session_recorded_timing,
    sqlite_query_filters,
};
use crate::config::RouteRequestLogBackend;

#[test]
fn model_candidates_paginate_all_models_and_apply_account_filters() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut entries = Vec::new();
    for index in 0..251 {
        let mut entry = sample_entry(&format!("request-{index}"));
        entry.timestamp_unix_ms = 1000;
        entry.model = Some(format!("model-{index:03}"));
        entry.official_account_id = Some("account-a".into());
        entries.push(queued(entry));
    }
    let mut other = sample_entry("other");
    other.timestamp_unix_ms = 1000;
    other.official_account_id = Some("account-b".into());
    entries.push(queued(other));
    sink.write_batch(&entries).unwrap();
    sink.finish().unwrap();
    let query = RouteRequestLogModelQuery {
        from_unix_ms: Some(1),
        to_unix_ms: Some(2000),
        provider: Some("provider-a".into()),
        official_account_id: Some("account-a".into()),
        after_model: None,
    };
    let first = query_route_request_log_models(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        query.clone(),
    )
    .unwrap();
    assert_eq!(first.models.len(), 200);
    assert_eq!(first.next_cursor.as_deref(), Some("model-199"));
    let next = query_route_request_log_models(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogModelQuery {
            after_model: first.next_cursor,
            ..query.clone()
        },
    )
    .unwrap();
    assert_eq!(next.models.len(), 51);
    assert_eq!(next.models.last().map(String::as_str), Some("model-250"));
    assert!(next.next_cursor.is_none());
    let unmatched = query_route_request_log_models(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogModelQuery {
            provider: Some("' OR 1=1 --".into()),
            ..query
        },
    )
    .unwrap();
    assert!(unmatched.models.is_empty());
}

#[test]
fn sqlite_cursor_and_range_statistics_preserve_boundaries_and_unknown_usage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut a = sample_entry("a");
    a.timestamp_unix_ms = 100;
    a.total_duration_ms = 0;
    a.downstream_first_content_ms = Some(0);
    a.token_usage.output_tokens = Some(150);
    a.token_usage.input_tokens = Some(5);
    a.token_usage.total_tokens = Some(10);
    a.ttft_ms = Some(0);
    let mut b = a.clone();
    b.request_id = "b".into();
    b.total_duration_ms = 900;
    b.status = RequestStatus::Failed;
    b.token_usage = RequestTokenUsage::default();
    b.token_usage.output_tokens = Some(300);
    b.usage_reported = false;
    b.downstream_first_content_ms = None;
    b.upstream_first_byte_ms = None;
    b.ttft_ms = Some(100);
    let mut c = sample_entry("c");
    c.timestamp_unix_ms = 100;
    c.total_duration_ms = 260;
    c.status = RequestStatus::Incomplete;
    c.token_usage.output_tokens = Some(160);
    c.token_usage.total_tokens = Some(20);
    c.downstream_first_content_ms = Some(60);
    c.router_pre_upstream_ms = Some(20);
    c.upstream_first_byte_ms = Some(40);
    c.ttft_ms = Some(60);
    let mut outside = sample_entry("outside");
    outside.timestamp_unix_ms = 200;
    sink.write_batch(&[queued(a), queued(b), queued(c.clone()), queued(outside)])
        .unwrap();
    sink.write_batch(&[queued(c)]).unwrap(); // Delivery replay must not duplicate statistics.
    let query = RouteRequestLogQuery {
        cursor_mode: true,
        page_size: 1,
        from_unix_ms: Some(100),
        to_unix_ms: Some(200),
        group_by: Some("status".into()),
        ..Default::default()
    };
    let first = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        query.clone(),
    )
    .unwrap();
    assert_eq!(first.items[0].request_id, "c");
    assert!(first.has_more);
    let second_query = RouteRequestLogQuery {
        cursor: first.next_cursor,
        ..query.clone()
    };
    let second = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        second_query.clone(),
    )
    .unwrap();
    assert_eq!(second.items[0].request_id, "b");
    // 明细行与统计口径一致：b 缺少上游首字节时间时速度保持未知。
    assert_eq!(second.items[0].output_tokens_per_second, None);
    assert_eq!(
        first.items[0].output_tokens_per_second,
        Some(160_000.0 / 200.0)
    );
    let third = query_route_request_logs(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            cursor: second.next_cursor,
            ..query.clone()
        },
    )
    .unwrap();
    assert_eq!(third.items[0].request_id, "a");
    assert!(!third.has_more);
    let stats = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        second_query,
    )
    .unwrap();
    assert_eq!(stats.summary.total, 3);
    assert_eq!(stats.summary.succeeded_count, 1);
    assert_eq!(stats.summary.failed_count, 1);
    assert_eq!(stats.summary.incomplete_count, 1);
    assert_eq!(stats.summary.avg_duration, Some(386.6666666666667));
    assert_eq!(stats.summary.avg_ttft, Some(53.333333333333336));
    // a 的计时逆序，b 缺少上游首字节时间，均被排除；只剩 c 参与汇总。
    assert_eq!(stats.summary.avg_output_tokens_per_second, Some(800.0));
    assert_eq!(stats.summary.input_tokens_sum, Some(15));
    assert_eq!(stats.summary.output_tokens_sum, Some(610));
    assert_eq!(stats.summary.total_tokens_sum, Some(30));
    assert_eq!(stats.summary.total_tokens_known_count, 2);
    assert_eq!(stats.summary.usage_reported_count, 2);
    assert_eq!(stats.groups.len(), 3);
    assert_eq!(stats.trend[0].total, 3);
    assert_eq!(
        stats.trend[0].avg_output_tokens_per_second,
        stats.summary.avg_output_tokens_per_second
    );
    let missing = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            request_id: Some("b".into()),
            ..query.clone()
        },
    )
    .unwrap();
    assert_eq!(missing.summary.total, 1);
    assert_eq!(missing.summary.total_tokens_sum, None);
    // 缺少上游首字节时间时保持未知，不回退到 ttft。
    assert_eq!(missing.summary.avg_output_tokens_per_second, None);
    let unknown_speed = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            request_id: Some("a".into()),
            ..query.clone()
        },
    )
    .unwrap();
    assert_eq!(unknown_speed.summary.avg_output_tokens_per_second, None);
    let empty = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            request_id: Some("absent".into()),
            ..query.clone()
        },
    )
    .unwrap();
    assert_eq!(empty.summary.total, 0);
    assert_eq!(empty.summary.avg_duration, None);
    assert_eq!(empty.summary.avg_output_tokens_per_second, None);
    assert_eq!(empty.summary.success_rate, None);
    let normalized = RouteRequestLogQuery {
        provider: Some("PROVIDER-A".into()),
        ..query
    }
    .normalize()
    .unwrap();
    let (filter, params) = sqlite_query_filters(&normalized, true);
    let connection = Connection::open(&path).unwrap();
    let plan = connection
        .prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT request_id FROM route_request_logs{filter}
        ORDER BY timestamp_unix_ms DESC, request_id DESC LIMIT 2"
        ))
        .unwrap()
        .query_map(params_from_iter(params.iter()), |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("idx_route_request_logs_provider_time"),
        "{plan}"
    );
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[test]
fn aggregate_output_speed_uses_total_tokens_over_total_generation_time() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut fast = sample_entry("fast");
    fast.total_duration_ms = 1_100;
    fast.downstream_first_content_ms = Some(100);
    fast.router_pre_upstream_ms = Some(20);
    fast.upstream_first_byte_ms = Some(80);
    fast.token_usage.output_tokens = Some(100);
    let mut slow = sample_entry("slow");
    slow.request_id = "slow".into();
    slow.trace_id = "slow".into();
    slow.total_duration_ms = 5_000;
    slow.downstream_first_content_ms = Some(1_000);
    slow.router_pre_upstream_ms = Some(200);
    slow.upstream_first_byte_ms = Some(800);
    slow.token_usage.output_tokens = Some(500);
    sink.write_batch(&[queued(fast), queued(slow)]).unwrap();

    let stats = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            from_unix_ms: Some(0),
            to_unix_ms: Some(10_000),
            ..Default::default()
        },
    )
    .unwrap();
    // fast 为 1 秒 100 token，slow 为 4 秒 500 token；汇总按总量相除，
    // 不像逐条求平均那样让短请求等权。
    assert_eq!(stats.summary.avg_output_tokens_per_second, Some(120.0));
}

#[test]
fn sqlite_daily_trend_preserves_utc_days_filters_and_unknown_usage() {
    let directory = tempfile::tempdir().unwrap();
    let mut sink = SqliteSink::open(&directory.path().join(SQLITE_FILE_NAME), 30).unwrap();
    let from = DAY_MS + 100;
    let to = 6 * DAY_MS + 100;
    let entries = [
        ("before", from - 1, Some(99)),
        ("first", from, Some(10)),
        ("mixed-unknown", 2 * DAY_MS - 1, None),
        ("all-unknown", 2 * DAY_MS, None),
        ("zero", 3 * DAY_MS, Some(0)),
        ("last", to - 1, Some(20)),
        ("after", to, Some(99)),
    ]
    .into_iter()
    .map(|(id, timestamp, tokens)| {
        let mut entry = sample_entry(id);
        entry.timestamp_unix_ms = timestamp;
        entry.token_usage.total_tokens = tokens;
        queued(entry)
    })
    .collect::<Vec<_>>();
    sink.write_batch(&entries).unwrap();
    let mut excluded = sample_entry("other-model");
    excluded.timestamp_unix_ms = from;
    excluded.model = Some("excluded".into());
    sink.write_batch(&[queued(excluded)]).unwrap();
    let query = RouteRequestLogQuery {
        from_unix_ms: Some(from),
        to_unix_ms: Some(to),
        model: sample_entry("model").model,
        include_daily_trend: true,
        ..Default::default()
    };
    let stats = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        query.clone(),
    )
    .unwrap();
    let actual = stats
        .daily_trend
        .iter()
        .map(|day| {
            (
                day.timestamp_unix_ms,
                day.total,
                day.total_tokens_sum,
                day.total_tokens_known_count,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (DAY_MS, 2, Some(10), 1),
            (2 * DAY_MS, 1, None, 0),
            (3 * DAY_MS, 1, Some(0), 1),
            (6 * DAY_MS, 1, Some(20), 1),
        ]
    );
    assert_eq!(
        stats.daily_trend.iter().map(|day| day.total).sum::<u64>(),
        stats.summary.total,
    );
    assert_eq!(
        Some(
            stats
                .daily_trend
                .iter()
                .filter_map(|day| day.total_tokens_sum)
                .sum::<u64>()
        ),
        stats.summary.total_tokens_sum,
    );
    assert_eq!(
        stats
            .daily_trend
            .iter()
            .map(|day| day.total_tokens_known_count)
            .sum::<u64>(),
        stats.summary.total_tokens_known_count,
    );
    let serialized = serde_json::to_value(&stats).unwrap();
    assert_eq!(
        serialized["dailyTrend"][1]["totalTokensSum"],
        serde_json::Value::Null,
    );
    assert_eq!(serialized["dailyTrend"][2]["totalTokensKnownCount"], 1);
    let without_daily = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            include_daily_trend: false,
            ..query.clone()
        },
    )
    .unwrap();
    assert!(without_daily.daily_trend.is_empty());
    assert_eq!(without_daily.bucket_ms, stats.bucket_ms);
    assert_eq!(
        serde_json::to_value(without_daily.trend).unwrap(),
        serialized["trend"],
    );
    let absent = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            request_id: Some("absent".into()),
            ..query
        },
    )
    .unwrap();
    assert!(absent.daily_trend.is_empty());
}

#[test]
fn sqlite_all_time_statistics_preserve_history_and_bound_buckets() {
    let directory = tempfile::tempdir().unwrap();
    let mut sink = SqliteSink::open(&directory.path().join(SQLITE_FILE_NAME), 30).unwrap();
    let now = unix_timestamp_ms();
    let earliest = now - 800 * DAY_MS;
    let entries = (0..800)
        .map(|index| {
            let mut entry = sample_entry(&format!("history-{index}"));
            entry.timestamp_unix_ms = earliest + index * DAY_MS;
            queued(entry)
        })
        .collect::<Vec<_>>();
    sink.write_batch(&entries).unwrap();
    let mut excluded = sample_entry("excluded-earlier-provider");
    excluded.timestamp_unix_ms = 1;
    excluded.provider = Some("other".into());
    sink.write_batch(&[queued(excluded)]).unwrap();
    let stats = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            all_time: true,
            provider: Some("provider-a".into()),
            include_daily_trend: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(stats.summary.total, 800);
    assert_eq!(stats.daily_trend.len(), 800);
    for (index, day) in stats.daily_trend.iter().enumerate() {
        assert_eq!(
            day.timestamp_unix_ms,
            earliest / DAY_MS * DAY_MS + index as u64 * DAY_MS,
        );
        assert_eq!(day.total, 1);
        assert_eq!(
            day.total_tokens_sum,
            sample_entry("expected").token_usage.total_tokens,
        );
        assert_eq!(day.total_tokens_known_count, 1);
    }
    assert_eq!(
        Some(
            stats
                .daily_trend
                .iter()
                .filter_map(|day| day.total_tokens_sum)
                .sum::<u64>()
        ),
        stats.summary.total_tokens_sum,
    );
    assert_eq!(stats.from_unix_ms, earliest);
    assert!(stats.to_unix_ms >= now);
    assert_eq!(stats.bucket_ms % DAY_MS, 0);
    assert!(stats.trend.len() <= 367);
    assert!(stats.to_unix_ms / stats.bucket_ms - stats.from_unix_ms / stats.bucket_ms < 367);
    assert_eq!(
        stats.trend.iter().map(|bucket| bucket.total).sum::<u64>(),
        800
    );
    assert!(
        query_route_request_log_stats(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                from_unix_ms: Some(earliest),
                to_unix_ms: Some(now),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn sqlite_token_group_order_applies_before_truncation_and_preserves_unknowns() {
    let directory = tempfile::tempdir().unwrap();
    let mut sink = SqliteSink::open(&directory.path().join(SQLITE_FILE_NAME), 30).unwrap();
    let mut entries = Vec::new();
    for model in 0..50 {
        for request in 0..2 {
            let mut entry = sample_entry(&format!("small-{model}-{request}"));
            entry.model = Some(format!("small-{model:02}"));
            entry.token_usage.total_tokens = Some(1);
            entries.push(queued(entry));
        }
    }
    let mut high = sample_entry("high");
    high.model = Some("high".into());
    high.status = RequestStatus::Failed;
    high.token_usage.total_tokens = Some(10_000);
    entries.push(queued(high));
    let mut unknown = sample_entry("unknown");
    unknown.model = Some("unknown".into());
    unknown.status = RequestStatus::Failed;
    unknown.token_usage = RequestTokenUsage::default();
    unknown.usage_reported = false;
    entries.push(queued(unknown));
    sink.write_batch(&entries).unwrap();
    let query = RouteRequestLogQuery {
        all_time: true,
        group_by: Some("model".into()),
        ..Default::default()
    };
    let default_order = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        query.clone(),
    )
    .unwrap();
    assert_eq!(default_order.groups[0].key, "small-00");
    assert!(default_order.groups.iter().all(|group| group.key != "high"));
    let tokens = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            group_sort: Some("tokens".into()),
            ..query.clone()
        },
    )
    .unwrap();
    assert_eq!(tokens.groups.len(), 50);
    assert!(tokens.groups_truncated);
    assert_eq!(tokens.groups[0].key, "high");
    assert_eq!(tokens.groups[1].key, "small-00");
    assert_eq!(tokens.summary.total_tokens_sum, Some(10_100));
    assert_eq!(tokens.summary.total_tokens_known_count, 101);
    let unknown = query_route_request_log_stats(
        directory.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery {
            group_sort: Some("tokens".into()),
            status: Some("failed".into()),
            ..query
        },
    )
    .unwrap();
    assert_eq!(unknown.groups[0].key, "high");
    assert_eq!(unknown.groups[1].key, "unknown");
    assert_eq!(unknown.groups[1].summary.total_tokens_sum, None);
}

#[test]
fn all_time_statistics_validate_parameters_and_do_not_create_database() {
    let directory = tempfile::tempdir().unwrap();
    let query: RouteRequestLogQuery = serde_json::from_value(serde_json::json!({
        "allTime": true, "groupSort": "tokens", "includeDailyTrend": true,
    }))
    .unwrap();
    assert!(query.all_time);
    assert!(query.include_daily_trend);
    assert!(!RouteRequestLogQuery::default().include_daily_trend);
    for backend in [
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogBackend::Ndjson,
    ] {
        let stats =
            query_route_request_log_stats(directory.path(), backend, query.clone()).unwrap();
        assert_eq!(stats.summary.total, 0);
        assert!(stats.trend.is_empty());
        assert!(stats.daily_trend.is_empty());
        assert!(stats.from_unix_ms < stats.to_unix_ms);
        assert_eq!(stats.queryable, backend == RouteRequestLogBackend::Sqlite);
    }
    assert!(!directory.path().join(SQLITE_FILE_NAME).exists());
    for stats_only in [
        RouteRequestLogQuery {
            all_time: true,
            ..Default::default()
        },
        RouteRequestLogQuery {
            group_sort: Some("tokens".into()),
            ..Default::default()
        },
        RouteRequestLogQuery {
            include_daily_trend: true,
            ..Default::default()
        },
    ] {
        assert!(
            query_route_request_logs(directory.path(), RouteRequestLogBackend::Sqlite, stats_only)
                .is_err()
        );
    }
    for invalid in [
        RouteRequestLogQuery {
            from_unix_ms: Some(1),
            ..query.clone()
        },
        RouteRequestLogQuery {
            to_unix_ms: Some(2),
            ..query.clone()
        },
        RouteRequestLogQuery {
            cursor_mode: true,
            ..query.clone()
        },
        RouteRequestLogQuery {
            cursor: Some(RouteRequestLogCursor {
                timestamp_unix_ms: 1,
                request_id: "one".into(),
            }),
            ..query.clone()
        },
        RouteRequestLogQuery {
            group_sort: Some("arbitrary".into()),
            ..query.clone()
        },
    ] {
        assert!(
            query_route_request_log_stats(
                directory.path(),
                RouteRequestLogBackend::Sqlite,
                invalid
            )
            .is_err()
        );
    }
    let _sink = SqliteSink::open(&directory.path().join(SQLITE_FILE_NAME), 30).unwrap();
    let empty =
        query_route_request_log_stats(directory.path(), RouteRequestLogBackend::Sqlite, query)
            .unwrap();
    assert_eq!(empty.summary.total, 0);
    assert!(empty.trend.is_empty());
    assert!(empty.daily_trend.is_empty());
    assert!(empty.from_unix_ms < empty.to_unix_ms);
}

#[test]
fn sqlite_analytics_rejects_invalid_ranges_and_grouping() {
    for query in [
        RouteRequestLogQuery {
            from_unix_ms: Some(200),
            to_unix_ms: Some(100),
            ..Default::default()
        },
        RouteRequestLogQuery {
            from_unix_ms: Some(0),
            to_unix_ms: Some(367 * DAY_MS),
            ..Default::default()
        },
        RouteRequestLogQuery {
            group_by: Some("model; DROP TABLE route_request_logs".into()),
            ..Default::default()
        },
        RouteRequestLogQuery {
            cursor: Some(RouteRequestLogCursor {
                timestamp_unix_ms: 1,
                request_id: "".into(),
            }),
            ..Default::default()
        },
    ] {
        assert!(query.normalize().is_err());
    }
}

#[test]
fn session_recorded_speed_uses_latest_eligible_own_request_without_creating_logs() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        latest_session_recorded_speed(directory.path(), "thread-sample"),
        None
    );
    let path = directory.path().join(SQLITE_FILE_NAME);
    assert!(!path.exists());
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let mut own = sample_entry("own");
    own.timestamp_unix_ms = 100;
    own.codex_session_id = Some("thread-sample".into());
    own.codex_session_is_parent = false;
    own.request_kind = "responses".into();
    own.token_usage.output_tokens = Some(200);
    own.total_duration_ms = 1100;
    own.router_pre_upstream_ms = Some(50);
    own.upstream_first_byte_ms = Some(50);
    let mut parent = own.clone();
    parent.request_id = "parent".into();
    parent.timestamp_unix_ms = 200;
    parent.codex_session_is_parent = true;
    let mut missing = own.clone();
    missing.request_id = "missing".into();
    missing.timestamp_unix_ms = 300;
    missing.upstream_first_byte_ms = None;
    let mut compact = own.clone();
    compact.request_id = "compact".into();
    compact.timestamp_unix_ms = 400;
    compact.request_kind = "responses_compact".into();
    sink.write_batch(&[
        queued(own),
        queued(parent),
        queued(missing),
        queued(compact),
    ])
    .unwrap();
    sink.finish().unwrap();
    drop(sink);
    assert_eq!(
        latest_session_recorded_speed(directory.path(), "thread-sample"),
        Some((200.0, 100))
    );
    assert_eq!(
        latest_session_recorded_speed(directory.path(), "foreign"),
        None
    );
    // 关闭写入后依然只读历史；兼容日志内带 local: 前缀的会话归属。
    let conn = Connection::open(&path).unwrap();
    conn.execute("UPDATE route_request_logs SET codex_session_id='local:thread-sample' WHERE request_id='own'", []).unwrap();
    assert_eq!(
        latest_session_recorded_speed(directory.path(), "thread-sample"),
        Some((200.0, 100))
    );
    for invalid_output in [0, 99] {
        conn.execute(
            "UPDATE route_request_logs SET output_tokens=?1 WHERE request_id='own'",
            [invalid_output],
        )
        .unwrap();
        assert_eq!(
            latest_session_recorded_speed(directory.path(), "thread-sample"),
            None
        );
    }
    conn.execute(
        "UPDATE route_request_logs SET output_tokens=200 WHERE request_id='own'",
        [],
    )
    .unwrap();
    // 异常时间不提供速度。
    conn.execute(
        "UPDATE route_request_logs SET router_pre_upstream_ms=-1 WHERE request_id='own'",
        [],
    )
    .unwrap();
    assert_eq!(
        latest_session_recorded_speed(directory.path(), "thread-sample"),
        None
    );
}

#[test]
fn session_recorded_timing_excludes_parent_compaction_and_missing_measurements() {
    let directory = tempfile::tempdir().unwrap();
    assert!(session_recorded_timing(directory.path(), "own", None).is_none());
    assert!(!directory.path().join(SQLITE_FILE_NAME).exists());
    let mut sink = SqliteSink::open(&directory.path().join(SQLITE_FILE_NAME), 30).unwrap();
    let mut entry = sample_entry("first");
    entry.codex_session_id = Some("own".into());
    entry.codex_session_is_parent = false;
    entry.request_kind = "responses".into();
    entry.total_duration_ms = 1500;
    entry.router_pre_upstream_ms = Some(500);
    entry.downstream_first_content_ms = Some(600);
    let mut missing = entry.clone();
    missing.request_id = "missing".into();
    missing.router_pre_upstream_ms = None;
    missing.downstream_first_content_ms = None;
    missing.ttft_ms = Some(5);
    let mut parent = entry.clone();
    parent.request_id = "parent".into();
    parent.codex_session_is_parent = true;
    let mut compact = entry.clone();
    compact.request_id = "compact".into();
    compact.request_kind = "responses_compact".into();
    let mut prefixed = entry.clone();
    prefixed.request_id = "prefixed".into();
    prefixed.codex_session_id = Some("local:own".into());
    prefixed.downstream_first_content_ms = Some(800);
    sink.write_batch(&[
        queued(entry),
        queued(missing),
        queued(parent),
        queued(compact),
        queued(prefixed),
    ])
    .unwrap();
    sink.finish().unwrap();
    drop(sink);
    let timing = session_recorded_timing(directory.path(), "own", None).unwrap();
    assert_eq!(timing.requests, 3);
    assert_eq!(timing.model_duration_ms, Some(2000));
    assert_eq!(timing.average_ttft_ms, Some(700.0));
    assert_eq!(timing.timing_request_count, 2);
    assert_eq!(timing.ttft_sample_count, 2);
    assert!(session_recorded_timing(directory.path(), "foreign", None).is_none());
    // 旧库缺计时字段时只返回未知，不迁移、不创建字段，也不依赖当前日志开关。
    let old = tempfile::tempdir().unwrap();
    let connection = Connection::open(old.path().join(SQLITE_FILE_NAME)).unwrap();
    connection.execute_batch("CREATE TABLE route_request_logs (codex_session_id TEXT, timestamp_unix_ms INTEGER);
        CREATE INDEX idx_route_request_logs_session_time ON route_request_logs(codex_session_id, timestamp_unix_ms);").unwrap();
    assert!(session_recorded_timing(old.path(), "own", None).is_none());
    let columns: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('route_request_logs')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 2);
}

#[test]
fn session_recorded_timing_filters_invalid_times_and_applies_runtime_boundary() {
    use crate::conversation_stats::RuntimeHistoryBoundary;
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::open(root.path().join(SQLITE_FILE_NAME)).unwrap();
    connection.execute_batch("CREATE TABLE route_request_logs (
        request_id TEXT PRIMARY KEY, codex_session_id TEXT, timestamp_unix_ms INTEGER,
        request_kind TEXT, codex_session_is_parent INTEGER, total_duration_ms INTEGER,
        router_pre_upstream_ms INTEGER, downstream_first_content_ms INTEGER);
        CREATE INDEX idx_route_request_logs_session_time ON route_request_logs(codex_session_id, timestamp_unix_ms);
        INSERT INTO route_request_logs VALUES
        ('old','own',100,'responses',0,1100,100,100),
        ('same-ms-old','local:own',200,'responses',0,2100,100,NULL),
        ('runtime','own',200,'responses',0,600,100,300),
        ('later','own',300,'responses',0,1100,100,100),
        ('invalid-timestamp','own',-1,'responses',0,1100,100,100),
        ('invalid-model','own',100,'responses',0,1000,-1,200),
        ('invalid-ttft','own',100,'responses',0,1000,100,1001),
        ('missing-both','own',100,'responses',0,1000,NULL,NULL);").unwrap();
    let boundary = RuntimeHistoryBoundary {
        timestamp_unix_ms: 200,
        request_ids: std::collections::HashSet::from(["runtime".into()]),
        exclude_entire_millisecond: false,
    };
    let timing = session_recorded_timing(root.path(), "own", Some(&boundary)).unwrap();
    assert_eq!(timing.requests, 5);
    assert_eq!(timing.model_duration_ms, Some(3900));
    assert_eq!(timing.timing_request_count, 3);
    assert_eq!(timing.average_ttft_ms, Some(150.0));
    assert_eq!(timing.ttft_sample_count, 2);
    let overflow = session_recorded_timing(
        root.path(),
        "own",
        Some(&RuntimeHistoryBoundary {
            exclude_entire_millisecond: true,
            ..boundary.clone()
        }),
    )
    .unwrap();
    assert_eq!(overflow.requests, 4);
    assert_eq!(overflow.model_duration_ms, Some(1900));
    let (sql, values) = super::session_timing_query("own", Some(&boundary));
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params_from_iter(values), |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("SEARCH route_request_logs USING INDEX idx_route_request_logs_session_time"),
        "{plan}"
    );
    assert!(plan.contains("timestamp_unix_ms<?"), "{plan}");
    assert!(!plan.contains("SCAN route_request_logs"), "{plan}");
}

#[test]
fn last_turn_stats_query_isolates_window_and_runtime_with_session_time_index() {
    use crate::conversation_stats::{RuntimeHistoryBoundary, TurnWindow};
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::open(root.path().join(SQLITE_FILE_NAME)).unwrap();
    connection.execute_batch("CREATE TABLE route_request_logs (
        request_id TEXT PRIMARY KEY, codex_session_id TEXT, timestamp_unix_ms INTEGER,
        request_kind TEXT, codex_session_is_parent INTEGER, total_duration_ms INTEGER,
        router_pre_upstream_ms INTEGER, downstream_first_content_ms INTEGER,
        upstream_first_byte_ms INTEGER, output_tokens INTEGER);
        CREATE INDEX idx_route_request_logs_session_time ON route_request_logs(codex_session_id, timestamp_unix_ms);
        INSERT INTO route_request_logs VALUES
        ('old','own',99,'responses',0,10000,100,200,200,9999),
        ('first','own',101,'responses',0,1300,100,300,200,100),
        ('second','local:own',200,'responses',0,3300,100,500,200,600),
        ('parent','own',200,'responses',1,1300,100,300,200,9999),
        ('compact','own',200,'responses_compact',0,1300,100,300,200,9999),
        ('runtime','own',300,'responses',0,1300,100,300,200,500),
        ('next','own',401,'responses',0,1300,100,300,200,9999),
        ('foreign','foreign',200,'responses',0,1300,100,300,200,9999);
        WITH RECURSIVE seq(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM seq WHERE x < 10000)
        INSERT INTO route_request_logs SELECT 'history-' || x,'own',0,'responses',0,1300,100,300,200,9999 FROM seq;").unwrap();
    let window = TurnWindow {
        id: "turn".into(),
        started_at_unix_ms: 100,
        completed_at_unix_ms: Some(400),
    };
    let boundary = RuntimeHistoryBoundary {
        timestamp_unix_ms: 300,
        request_ids: std::collections::HashSet::from(["runtime".into()]),
        exclude_entire_millisecond: false,
    };
    let totals =
        super::last_turn_recorded_stats(root.path(), "own", &window, Some(&boundary)).unwrap();
    assert_eq!(totals.requests, 2);
    assert_eq!(totals.excluded_boundary_requests, 0);
    assert_eq!(totals.generation.speed(), Some(175.0));
    assert_eq!(totals.model_duration_ms, 4400);
    assert_eq!(totals.ttft_total_ms / totals.ttft_samples as f64, 400.0);
    let (sql, values) = super::last_turn_stats_query("own", &window, Some(&boundary));
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params_from_iter(values), |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("SEARCH route_request_logs USING INDEX idx_route_request_logs_session_time"),
        "{plan}"
    );
    assert!(plan.contains("timestamp_unix_ms>?"), "{plan}");
    assert!(plan.contains("timestamp_unix_ms<?"), "{plan}");
    assert!(!plan.contains("SCAN route_request_logs"), "{plan}");
    connection
        .execute("DROP INDEX idx_route_request_logs_session_time", [])
        .unwrap();
    assert!(super::last_turn_recorded_stats(root.path(), "own", &window, None).is_none());
}

#[test]
fn last_turn_stats_excludes_boundary_milliseconds_without_hiding_partial_coverage() {
    use crate::conversation_stats::{RuntimeHistoryBoundary, TurnWindow};
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::open(root.path().join(SQLITE_FILE_NAME)).unwrap();
    connection.execute_batch("CREATE TABLE route_request_logs (
        request_id TEXT PRIMARY KEY, codex_session_id TEXT, timestamp_unix_ms INTEGER,
        request_kind TEXT, codex_session_is_parent INTEGER, total_duration_ms INTEGER,
        router_pre_upstream_ms INTEGER, downstream_first_content_ms INTEGER,
        upstream_first_byte_ms INTEGER, output_tokens INTEGER);
        CREATE INDEX idx_route_request_logs_session_time ON route_request_logs(codex_session_id, timestamp_unix_ms);
        INSERT INTO route_request_logs VALUES
        ('previous-turn','own',100,'responses',0,1300,100,300,200,9000),
        ('possibly-new-turn','own',100,'responses',0,1300,100,300,200,100),
        ('inside','own',101,'responses',0,1300,100,300,200,200),
        ('end-boundary','local:own',400,'responses',0,1300,100,300,200,9000),
        ('parent-boundary','own',100,'responses',1,1300,100,300,200,9000),
        ('compact-boundary','own',400,'responses_compact',0,1300,100,300,200,9000),
        ('foreign-boundary','foreign',100,'responses',0,1300,100,300,200,9000);").unwrap();
    let mut window = TurnWindow {
        id: "turn".into(),
        started_at_unix_ms: 100,
        completed_at_unix_ms: Some(400),
    };
    // 结束边界晚于运行期接管时间，仍须在同一查询内报告其归属不明。
    let boundary = RuntimeHistoryBoundary {
        timestamp_unix_ms: 300,
        request_ids: std::collections::HashSet::from(["runtime".into()]),
        exclude_entire_millisecond: false,
    };
    let totals =
        super::last_turn_recorded_stats(root.path(), "own", &window, Some(&boundary)).unwrap();
    assert_eq!(totals.requests, 1);
    assert_eq!(totals.model_duration_ms, 1200);
    assert_eq!(totals.generation.speed(), Some(200.0));
    assert_eq!(totals.excluded_boundary_requests, 3);

    connection
        .execute(
            "DELETE FROM route_request_logs WHERE request_id = 'inside'",
            [],
        )
        .unwrap();
    let only_boundaries =
        super::last_turn_recorded_stats(root.path(), "own", &window, Some(&boundary)).unwrap();
    assert_eq!(only_boundaries.requests, 0);
    assert_eq!(only_boundaries.model_samples, 0);
    assert_eq!(only_boundaries.ttft_samples, 0);
    assert_eq!(only_boundaries.generation.speed(), None);
    assert_eq!(only_boundaries.excluded_boundary_requests, 3);

    window.completed_at_unix_ms = Some(100);
    let same_millisecond =
        super::last_turn_recorded_stats(root.path(), "own", &window, None).unwrap();
    assert_eq!(same_millisecond.requests, 0);
    assert_eq!(same_millisecond.excluded_boundary_requests, 2);
}

#[test]
fn session_recorded_average_filters_invalid_rows_and_deduplicates_runtime_boundary() {
    use super::{session_average_query, session_recorded_average};
    use crate::conversation_stats::RuntimeHistoryBoundary;
    let directory = tempfile::tempdir().unwrap();
    assert!(session_recorded_average(directory.path(), "own", None).is_none());
    assert!(!directory.path().join(SQLITE_FILE_NAME).exists());
    let mut sink = SqliteSink::open(&directory.path().join(SQLITE_FILE_NAME), 30).unwrap();
    let mut own = sample_entry("old");
    own.codex_session_id = Some("own".into());
    own.codex_session_is_parent = false;
    own.request_kind = "responses".into();
    own.timestamp_unix_ms = 100;
    own.token_usage.output_tokens = Some(10);
    own.router_pre_upstream_ms = Some(100);
    own.upstream_first_byte_ms = Some(200);
    own.total_duration_ms = 1_300;
    let mut entries = vec![queued(own.clone())];
    for id in [
        "same-ms-old",
        "runtime",
        "later",
        "parent",
        "compact",
        "foreign",
        "missing-usage",
        "missing-time",
        "invalid",
        "zero-duration",
        "zero-output",
    ] {
        let mut entry = own.clone();
        entry.request_id = id.into();
        entry.timestamp_unix_ms = 200;
        match id {
            "same-ms-old" => {
                entry.codex_session_id = Some("local:own".into());
                entry.token_usage.output_tokens = Some(30);
            }
            "runtime" => entry.token_usage.output_tokens = Some(900),
            "later" => entry.timestamp_unix_ms = 201,
            "parent" => entry.codex_session_is_parent = true,
            "compact" => entry.request_kind = "responses_compact".into(),
            "foreign" => entry.codex_session_id = Some("foreign".into()),
            "missing-usage" => entry.token_usage.output_tokens = None,
            "missing-time" => entry.router_pre_upstream_ms = None,
            "invalid" => entry.upstream_first_byte_ms = Some(5_000),
            "zero-duration" => entry.total_duration_ms = 300,
            "zero-output" => {
                entry.token_usage.output_tokens = Some(0);
                entry.timestamp_unix_ms = 100;
            }
            _ => {}
        }
        entries.push(queued(entry));
    }
    sink.write_batch(&entries).unwrap();
    sink.finish().unwrap();
    drop(sink);
    let boundary = RuntimeHistoryBoundary {
        timestamp_unix_ms: 200,
        request_ids: std::collections::HashSet::from(["runtime".to_owned()]),
        exclude_entire_millisecond: false,
    };
    let totals = session_recorded_average(directory.path(), "own", Some(&boundary)).unwrap();
    assert_eq!(totals.output_tokens, 40.0);
    assert_eq!(totals.generation_ms, 3_000.0);
    assert_eq!(totals.requests, 3);
    assert_eq!(totals.speed(), Some(40.0 / 3.0));
    let old_only = session_recorded_average(
        directory.path(),
        "own",
        Some(&RuntimeHistoryBoundary {
            exclude_entire_millisecond: true,
            ..boundary.clone()
        }),
    )
    .unwrap();
    assert_eq!(old_only.requests, 2);
    assert_eq!(old_only.speed(), Some(5.0));
    let all = session_recorded_average(directory.path(), "own", None).unwrap();
    assert_eq!(all.requests, 5);
    assert_eq!(all.output_tokens, 950.0);
    let connection = Connection::open(directory.path().join(SQLITE_FILE_NAME)).unwrap();
    connection
        .execute(
            "UPDATE route_request_logs SET router_pre_upstream_ms=-1 WHERE request_id='old'",
            [],
        )
        .unwrap();
    assert_eq!(
        session_recorded_average(directory.path(), "own", Some(&boundary))
            .unwrap()
            .requests,
        2
    );
    let (sql, values) = session_average_query("own", Some(&boundary));
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params_from_iter(values), |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("SEARCH route_request_logs USING INDEX idx_route_request_logs_session_time"),
        "{plan}"
    );
    assert!(plan.contains("timestamp_unix_ms<?"), "{plan}");
    assert!(!plan.contains("SCAN route_request_logs"), "{plan}");
}

#[test]
fn session_recorded_average_is_read_only_for_old_schema() {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join(SQLITE_FILE_NAME)).unwrap();
    connection.execute_batch("CREATE TABLE route_request_logs (codex_session_id TEXT, timestamp_unix_ms INTEGER);
        CREATE INDEX idx_route_request_logs_session_time ON route_request_logs(codex_session_id, timestamp_unix_ms);").unwrap();
    assert!(super::session_recorded_average(directory.path(), "own", None).is_none());
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('route_request_logs')",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        2
    );
}

#[test]
fn session_average_query_uses_index_with_large_unrelated_history() {
    use super::{session_average_query, session_recorded_average};
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join(SQLITE_FILE_NAME)).unwrap();
    connection.execute_batch("CREATE TABLE route_request_logs (
        request_id TEXT PRIMARY KEY, codex_session_id TEXT, timestamp_unix_ms INTEGER,
        request_kind TEXT, codex_session_is_parent INTEGER, output_tokens INTEGER,
        total_duration_ms INTEGER, router_pre_upstream_ms INTEGER, upstream_first_byte_ms INTEGER);
        CREATE INDEX idx_route_request_logs_session_time ON route_request_logs(codex_session_id, timestamp_unix_ms);
        WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<99999)
        INSERT INTO route_request_logs SELECT 'other-'||x, 'other-'||(x%1000), x, 'responses', 0, 100, 1300, 100, 200 FROM n;
        WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<9999)
        INSERT INTO route_request_logs SELECT 'own-'||x, 'own', x, 'responses', 0, 100, 1300, 100, 200 FROM n;").unwrap();
    let boundary = crate::conversation_stats::RuntimeHistoryBoundary {
        timestamp_unix_ms: 5000,
        request_ids: std::collections::HashSet::from(["own-5000".to_owned()]),
        exclude_entire_millisecond: false,
    };
    let started = std::time::Instant::now();
    let totals = session_recorded_average(directory.path(), "own", Some(&boundary)).unwrap();
    eprintln!(
        "session average: 100000 unrelated + 10000 own rows, indexed aggregate of 5000 rows: {:?}",
        started.elapsed()
    );
    assert_eq!(totals.requests, 5000);
    assert_eq!(totals.speed(), Some(100.0));
    let (sql, params) = session_average_query("own", Some(&boundary));
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params_from_iter(params), |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("SEARCH route_request_logs USING INDEX idx_route_request_logs_session_time"),
        "{plan}"
    );
    assert!(plan.contains("timestamp_unix_ms<?"), "{plan}");
    assert!(!plan.contains("SCAN route_request_logs"), "{plan}");
}

#[test]
fn sqlite_query_paginates_in_reverse_time_order() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
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
    let mut sink = SqliteSink::open(&path, 30).unwrap();
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
            protocol: Some("HTTP_SSE".into()),
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
fn sqlite_protocol_filter_uses_upstream_transport() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 30).unwrap();
    let entries = [
        ("http", Some(UpstreamTransport::Http)),
        ("http_sse", Some(UpstreamTransport::HttpSse)),
        ("ws", Some(UpstreamTransport::WebSocket)),
        ("not-sent", None),
    ]
    .map(|(id, transport)| {
        let mut entry = sample_entry(id);
        entry.request_protocol = RequestProtocol::WebSocket;
        entry.upstream_transport = transport;
        queued(entry)
    });
    sink.write_batch(&entries).unwrap();
    sink.finish().unwrap();
    drop(sink);

    for protocol in ["http", "http_sse", "ws"] {
        let page = query_route_request_logs(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            RouteRequestLogQuery {
                protocol: Some(protocol.into()),
                ..RouteRequestLogQuery::default()
            },
        )
        .unwrap();
        assert_eq!(page.total, 1, "{protocol}");
        assert_eq!(page.items[0].request_id, protocol);
        assert_eq!(page.items[0].upstream_transport.as_deref(), Some(protocol));
    }
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
        route_decision: None,
        upstream_attempts: Vec::new(),
        attempts_omitted: 0,
        requested_service_tier: None,
        service_tier: None,
        request_id: "request".into(),
        trace_id: "trace".into(),
        timestamp_unix_ms: 1,
        provider: None,
        provider_name: None,
        official_account_id: None,
        requested_model: "requested".into(),
        model: None,
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
        output_tokens_per_second: None,
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
        fallback_count: 0,
        fallback_reason: None,
        upstream_authority: None,
        upstream_request_headers: None,
        upstream_response_headers: None,
        upstream_request_id: None,
        upstream_protocol: None,
        protocol_bridge: None,
        request_input_state: Some("array".into()),
        request_input_items: Some(3),
        request_has_previous_response_id: Some(false),
        request_bytes: Some(2_048),
        upstream_input_state: Some("array".into()),
        upstream_input_items: Some(3),
        upstream_has_previous_response_id: None,
        upstream_bytes: None,
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
    assert_eq!(value["requestInputState"], "array");
    assert_eq!(value["requestInputItems"], 3);
    assert_eq!(value["requestBytes"], 2_048);
    assert!(
        value
            .get("upstreamHasPreviousResponseId")
            .unwrap()
            .is_null()
    );
    assert!(value.get("upstreamBytes").unwrap().is_null());
    assert!(value.get("routerPreUpstreamMs").unwrap().is_null());
    assert!(value.get("upstreamFirstByteMs").unwrap().is_null());
    assert!(value.get("downstreamFirstContentMs").unwrap().is_null());
    assert!(value.get("outputTokensPerSecond").unwrap().is_null());
    // 上游未回报实际使用模型时该字段为空。
    assert!(value.get("upstreamResponseModel").unwrap().is_null());
    assert!(value.get("request_id").is_none());
    assert!(value.get("retryCount").is_none());
}

#[test]
fn clear_only_removes_exact_request_log_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let removable = [
        SQLITE_FILE_NAME,
        "route-requests.sqlite3-wal",
        "route-requests.sqlite3-shm",
        "route-requests.sqlite3-journal",
        NDJSON_FILE_NAME,
        "route-requests.ndjson.1",
        "route-requests.ndjson.27",
    ];
    let preserved = [
        "route-requests.sqlite3.backup",
        "route-requests.sqlite3-wal.backup",
        "route-requests.ndjson.0",
        "route-requests.ndjson.01",
        "route-requests.ndjson.backup",
        "route-requests.ndjson.2.tmp",
        "unrelated.ndjson.1",
    ];
    for file_name in removable.iter().chain(preserved.iter()) {
        fs::write(directory.path().join(file_name), b"test").unwrap();
    }

    let result = clear_route_request_log_files(directory.path(), false);

    assert_eq!(result.status, "ok");
    assert_eq!(result.removed_file_count, removable.len());
    for file_name in removable {
        assert!(!directory.path().join(file_name).exists(), "{file_name}");
    }
    for file_name in preserved {
        assert!(directory.path().join(file_name).exists(), "{file_name}");
    }
}

#[test]
fn clear_missing_or_empty_directory_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");

    let first = clear_route_request_log_files(&missing, false);
    let second = clear_route_request_log_files(&missing, false);

    assert_eq!(first.status, "ok");
    assert_eq!(first.removed_file_count, 0);
    assert_eq!(second, first);
}
