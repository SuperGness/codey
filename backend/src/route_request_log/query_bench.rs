//! 【自动化测试】请求日志统计与优化前独立聚合的结果对照，以及可选的本地性能对比。

use std::path::Path;
use std::time::Instant;

use super::super::test_support::{queued, sample_entry};
use super::super::writer::SqliteSink;
use super::super::{
    RequestStatus, RouteRequestLogDailyTrend, RouteRequestLogGroup, RouteRequestLogQuery,
    RouteRequestLogSummary, RouteRequestLogTrend, SQLITE_FILE_NAME,
};
use super::{
    DAY_MS, OUTPUT_SPEED_SUM_SQL, open_query_connection, query_route_request_log_stats,
    sqlite_optional_columns, sqlite_query_filters, summary_columns, summary_from_row,
};
use crate::config::RouteRequestLogBackend;

type BaselineStatistics = (
    RouteRequestLogSummary,
    Vec<RouteRequestLogGroup>,
    Vec<RouteRequestLogTrend>,
    Vec<RouteRequestLogDailyTrend>,
);

/// 按优化前的独立聚合顺序执行，供同一临时数据集上的前后对比使用。
fn query_baseline(root: &Path, query: &RouteRequestLogQuery) -> BaselineStatistics {
    let path = root.join(SQLITE_FILE_NAME);
    let mut connection = open_query_connection(&path).unwrap();
    let transaction = connection.transaction().unwrap();
    let optional_columns = sqlite_optional_columns(&transaction, &path).unwrap();
    let (where_clause, values) = sqlite_query_filters(query, optional_columns.official_account);
    let columns = summary_columns();
    let summary = transaction
        .query_row(
            &format!("SELECT {columns} FROM route_request_logs{where_clause}"),
            rusqlite::params_from_iter(values.iter()),
            summary_from_row,
        )
        .unwrap();
    let groups = if query.group_by.as_deref() == Some("model") {
        transaction
            .prepare(&format!(
                "SELECT {columns}, COALESCE(model, requested_model) AS group_key
                FROM route_request_logs{where_clause} GROUP BY group_key
                ORDER BY COUNT(*) DESC, group_key LIMIT 51"
            ))
            .unwrap()
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                Ok(RouteRequestLogGroup {
                    key: row.get(19)?,
                    summary: summary_from_row(row)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    } else {
        Vec::new()
    };
    let bucket_ms = DAY_MS / 24;
    let trend = transaction
        .prepare(&format!(
            "SELECT timestamp_unix_ms / {bucket_ms} * {bucket_ms} AS bucket,
            COUNT(*), SUM(total_tokens),
            AVG(CASE WHEN total_duration_ms >= 0 THEN total_duration_ms END),
            AVG(CASE WHEN COALESCE(downstream_first_content_ms, ttft_ms) >= 0
                THEN COALESCE(downstream_first_content_ms, ttft_ms) END),
            {OUTPUT_SPEED_SUM_SQL},
            AVG(CASE WHEN downstream_first_content_ms >= 0
                THEN downstream_first_content_ms END)
            FROM route_request_logs{where_clause} GROUP BY bucket ORDER BY bucket"
        ))
        .unwrap()
        .query_map(rusqlite::params_from_iter(values.iter()), |row| {
            Ok(RouteRequestLogTrend {
                timestamp_unix_ms: super::row_u64(row, 0)?,
                total: super::row_u64(row, 1)?,
                total_tokens_sum: super::row_optional_u64(row, 2)?,
                avg_duration: row.get(3)?,
                avg_ttft: row.get(4)?,
                avg_output_tokens_per_second: row.get(5)?,
                avg_downstream_first_content: row.get(6)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let daily_trend = transaction
        .prepare(&format!(
            "SELECT timestamp_unix_ms / {DAY_MS} * {DAY_MS} AS day,
            COUNT(*), SUM(total_tokens), COUNT(total_tokens)
            FROM route_request_logs{where_clause} GROUP BY day ORDER BY day"
        ))
        .unwrap()
        .query_map(rusqlite::params_from_iter(values.iter()), |row| {
            Ok(RouteRequestLogDailyTrend {
                timestamp_unix_ms: super::row_u64(row, 0)?,
                total: super::row_u64(row, 1)?,
                total_tokens_sum: super::row_optional_u64(row, 2)?,
                total_tokens_known_count: super::row_u64(row, 3)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    transaction.commit().unwrap();
    (summary, groups, trend, daily_trend)
}

/// 写入覆盖 7 天、5 条线路、20 个模型和 3 个账号的记录，返回临时目录与时间范围。
fn seed(rows: usize) -> (tempfile::TempDir, u64, u64) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(SQLITE_FILE_NAME);
    let mut sink = SqliteSink::open(&path, 365).unwrap();
    let from = 2_000 * DAY_MS;
    let to = from + 7 * DAY_MS;
    for chunk_start in (0..rows).step_by(1_000) {
        let chunk_end = (chunk_start + 1_000).min(rows);
        let entries = (chunk_start..chunk_end)
            .map(|index| {
                let mut entry = sample_entry(&format!("bench-{index}"));
                entry.timestamp_unix_ms = from + (index as u64 * 7_919) % (7 * DAY_MS);
                entry.provider = Some(format!("provider-{}", index % 5));
                entry.model = Some(format!("model-{}", index % 20));
                entry.official_account_id = Some(format!("account-{}", index % 3));
                entry.total_duration_ms = 1_000 + (index % 5_000) as u64;
                entry.router_pre_upstream_ms = Some((index % 100) as u64);
                entry.upstream_first_byte_ms = Some(100 + (index % 300) as u64);
                entry.downstream_first_content_ms = Some(200 + (index % 500) as u64);
                entry.token_usage.output_tokens = Some(100 + (index % 900) as u64);
                entry.token_usage.total_tokens = Some((index % 1_000) as u64);
                entry.status = if index % 17 == 0 {
                    RequestStatus::Failed
                } else {
                    RequestStatus::Succeeded
                };
                queued(entry)
            })
            .collect::<Vec<_>>();
        sink.write_batch(&entries).unwrap();
    }
    sink.finish().unwrap();
    drop(sink);
    (directory, from, to)
}

fn statistics_query(from: u64, to: u64) -> RouteRequestLogQuery {
    RouteRequestLogQuery {
        from_unix_ms: Some(from),
        to_unix_ms: Some(to),
        group_by: Some("model".into()),
        include_daily_trend: true,
        ..Default::default()
    }
}

fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap()
}

/// 统计结构只派生 Serialize，按 JSON 值逐项比较新旧聚合。
fn assert_matches_baseline(root: &Path, query: &RouteRequestLogQuery) {
    let current =
        query_route_request_log_stats(root, RouteRequestLogBackend::Sqlite, query.clone()).unwrap();
    let (summary, groups, trend, daily_trend) = query_baseline(root, query);
    assert_eq!(json(&current.summary), json(&summary));
    assert_eq!(json(&current.groups), json(&groups));
    assert_eq!(json(&current.trend), json(&trend));
    assert_eq!(json(&current.daily_trend), json(&daily_trend));
}

#[test]
fn statistics_match_independent_aggregates() {
    let (directory, from, to) = seed(3_000);
    let query = statistics_query(from, to);
    assert_matches_baseline(directory.path(), &query);
    assert_matches_baseline(
        directory.path(),
        &RouteRequestLogQuery {
            official_account_id: Some("account-1".into()),
            ..query
        },
    );
}

#[test]
#[ignore = "opt-in request-log statistics benchmark"]
fn statistics_performance() {
    const ROWS: usize = 100_000;
    const RUNS: usize = 20;

    let (directory, from, to) = seed(ROWS);
    let query = statistics_query(from, to);
    assert_matches_baseline(directory.path(), &query);
    let current = || {
        query_route_request_log_stats(
            directory.path(),
            RouteRequestLogBackend::Sqlite,
            query.clone(),
        )
        .unwrap()
    };
    let baseline = || query_baseline(directory.path(), &query);
    let current_warmup = current();
    let baseline_warmup = baseline();
    assert_eq!(current_warmup.summary.total, ROWS as u64);
    assert_eq!(current_warmup.groups.len(), 20);
    assert_eq!(baseline_warmup.0.total, ROWS as u64);
    assert_eq!(baseline_warmup.1.len(), 20);
    let mut current_samples = Vec::with_capacity(RUNS);
    let mut baseline_samples = Vec::with_capacity(RUNS);
    for run in 0..RUNS {
        let current_first = run % 2 == 0;
        if current_first {
            let started = Instant::now();
            let stats = current();
            current_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(stats.summary.total, ROWS as u64);
            assert_eq!(stats.daily_trend.len(), 7);
            let started = Instant::now();
            let stats = baseline();
            baseline_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(stats.0.total, ROWS as u64);
        } else {
            let started = Instant::now();
            let stats = baseline();
            baseline_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(stats.0.total, ROWS as u64);
            let started = Instant::now();
            let stats = current();
            current_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(stats.summary.total, ROWS as u64);
        }
    }
    current_samples.sort_by(f64::total_cmp);
    baseline_samples.sort_by(f64::total_cmp);
    let percentile = |samples: &[f64], percent: usize| {
        samples[(samples.len() * percent).div_ceil(100).saturating_sub(1)]
    };
    let current_median = percentile(&current_samples, 50);
    let baseline_median = percentile(&baseline_samples, 50);
    eprintln!(
        "route-log stats benchmark: rows={ROWS}, runs={RUNS}, \
         baseline_median_ms={baseline_median:.3}, current_median_ms={current_median:.3}, \
         median_reduction_pct={:.1}, baseline_p95_ms={:.3}, current_p95_ms={:.3}",
        (baseline_median - current_median) * 100.0 / baseline_median,
        percentile(&baseline_samples, 95),
        percentile(&current_samples, 95),
    );
}
