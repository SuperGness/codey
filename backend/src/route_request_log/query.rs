//! 日志查询、统计与文件清理；查询连接保持只读并兼容历史 schema。

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
use rusqlite::params;
use rusqlite::{
    Connection, OpenFlags, functions::FunctionFlags, params_from_iter, types::Value as SqlValue,
};

#[cfg(test)]
use super::SessionRecordedTiming;
use super::{
    MAX_LOG_STRING_BYTES, NDJSON_FILE_NAME, RouteRequestLogAnalytics, RouteRequestLogClearResult,
    RouteRequestLogCursor, RouteRequestLogDailyTrend, RouteRequestLogGroup,
    RouteRequestLogModelPage, RouteRequestLogModelQuery, RouteRequestLogQuery,
    RouteRequestLogQueryItem, RouteRequestLogQueryPage, RouteRequestLogSummary,
    RouteRequestLogTrend, SQLITE_BUSY_TIMEOUT, SQLITE_FILE_NAME, SessionGenerationTotals,
    lock_unpoisoned, to_i64, unix_timestamp_ms,
};
use crate::config::RouteRequestLogBackend;
use crate::sqlite_util::table_columns;

const DEFAULT_QUERY_PAGE_SIZE: u64 = 25;
const MAX_QUERY_PAGE: u64 = 1_000_000;
const MAX_QUERY_PAGE_SIZE: u64 = 100;
const MAX_QUERY_SEARCH_BYTES: usize = 256;
const MAX_QUERY_FILTER_BYTES: usize = 128;
const DAY_MS: u64 = 86_400_000;

/// 只接受配置或路由快照提供的别名，不能按斜杠猜测模型来源。
#[derive(Default)]
pub(crate) struct RouteRequestLogModelAliases {
    sources: HashMap<String, String>,
}

impl RouteRequestLogModelAliases {
    pub(crate) fn new(
        current_aliases: impl IntoIterator<Item = (String, String)>,
        raw_models: impl IntoIterator<Item = String>,
        historical_aliases: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let raw_models = raw_models
            .into_iter()
            .map(|model| crate::model_id::key(&model))
            .collect::<HashSet<_>>();
        let mut sources = historical_aliases
            .into_iter()
            .filter(|(alias, source)| {
                !alias.trim().is_empty()
                    && !source.trim().is_empty()
                    && !raw_models.contains(&crate::model_id::key(alias))
            })
            .map(|(alias, source)| (crate::model_id::key(&alias), source.trim().to_owned()))
            .collect::<HashMap<_, _>>();
        // 与路由选择一致：当前别名优先，其次是原始模型，最后才是历史别名。
        sources.extend(current_aliases.into_iter().filter_map(|(alias, source)| {
            (!alias.trim().is_empty() && !source.trim().is_empty())
                .then(|| (crate::model_id::key(&alias), source.trim().to_owned()))
        }));
        Self { sources }
    }

    fn source<'a>(&'a self, selector: &'a str) -> &'a str {
        self.sources
            .get(&crate::model_id::key(selector))
            .map_or(selector, String::as_str)
    }

    fn register(self, connection: &Connection) -> rusqlite::Result<()> {
        connection.create_scalar_function(
            "route_request_log_model",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| {
                let model = context.get::<Option<String>>(0)?;
                let requested_model = context.get::<String>(1)?;
                // 实际上游模型不能再次解别名。历史图片请求可能把选择器同时
                // 写入两列，提前失败的请求则只有 requested_model。
                if let Some(model) = &model
                    && !crate::model_id::equal(model, &requested_model)
                {
                    return Ok(model.clone());
                }
                Ok(self
                    .source(model.as_deref().unwrap_or(&requested_model))
                    .to_owned())
            },
        )
    }
}

/// 上游响应阶段每秒输出 Token 数，口径与写入侧的 `output_tokens_per_second` 一致。
/// 上游首字节时间以发送上游请求为起点，先加路由前置耗时，再从请求总耗时扣除。
/// 分步减法及非负校验避免非法历史计时与整数加法溢出，条件不足时返回 NULL。
const OUTPUT_SPEED_SQL: &str = "CASE
        WHEN total_duration_ms >= 0
            AND router_pre_upstream_ms >= 0
            AND upstream_first_byte_ms >= 0
            AND router_pre_upstream_ms <= total_duration_ms
            AND upstream_first_byte_ms <= total_duration_ms - router_pre_upstream_ms
            AND output_tokens >= 100
            AND total_duration_ms - router_pre_upstream_ms - upstream_first_byte_ms >= 100
        THEN output_tokens * 1000.0 / (total_duration_ms - router_pre_upstream_ms - upstream_first_byte_ms)
    END";

/// 汇总速度与单条速度口径一致，但按合格请求的输出总量除以生成总时长，
/// 逐条取平均会被短请求带偏。
const OUTPUT_SPEED_SUM_SQL: &str = "SUM(CASE
        WHEN total_duration_ms >= 0
            AND router_pre_upstream_ms >= 0
            AND upstream_first_byte_ms >= 0
            AND router_pre_upstream_ms <= total_duration_ms
            AND upstream_first_byte_ms <= total_duration_ms - router_pre_upstream_ms
            AND output_tokens >= 100
            AND total_duration_ms - router_pre_upstream_ms - upstream_first_byte_ms >= 100
        THEN output_tokens END) * 1000.0 / SUM(CASE
        WHEN total_duration_ms >= 0
            AND router_pre_upstream_ms >= 0
            AND upstream_first_byte_ms >= 0
            AND router_pre_upstream_ms <= total_duration_ms
            AND upstream_first_byte_ms <= total_duration_ms - router_pre_upstream_ms
            AND output_tokens >= 100
            AND total_duration_ms - router_pre_upstream_ms - upstream_first_byte_ms >= 100
        THEN total_duration_ms - router_pre_upstream_ms - upstream_first_byte_ms END)";

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
            from_unix_ms: None,
            to_unix_ms: None,
            cursor_mode: false,
            cursor: None,
            request_id: None,
            request_kind: None,
            session_id: None,
            official_account_id: None,
            group_by: None,
            all_time: false,
            group_sort: None,
            include_daily_trend: false,
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
        normalize_query_value(&mut self.request_id, MAX_LOG_STRING_BYTES, "请求 ID")?;
        normalize_query_value(&mut self.request_kind, MAX_QUERY_FILTER_BYTES, "请求类型")?;
        normalize_query_value(&mut self.session_id, MAX_LOG_STRING_BYTES, "会话 ID")?;
        normalize_query_value(
            &mut self.official_account_id,
            MAX_LOG_STRING_BYTES,
            "官方账号",
        )?;
        normalize_query_value(&mut self.group_by, MAX_QUERY_FILTER_BYTES, "统计维度")?;
        normalize_query_value(&mut self.group_sort, MAX_QUERY_FILTER_BYTES, "统计排序")?;
        if self
            .group_sort
            .as_deref()
            .is_some_and(|value| value != "tokens")
        {
            anyhow::bail!("统计排序无效");
        }
        if self.group_by.as_deref().is_some_and(|value| {
            !matches!(
                value,
                "model"
                    | "provider"
                    | "status"
                    | "protocol"
                    | "request_kind"
                    | "session"
                    | "official_account"
            )
        }) {
            anyhow::bail!("统计维度无效");
        }
        if self.cursor_mode || self.from_unix_ms.is_some() || self.to_unix_ms.is_some() {
            let to = self.to_unix_ms.unwrap_or_else(unix_timestamp_ms);
            let from = self
                .from_unix_ms
                .unwrap_or_else(|| to.saturating_sub(DAY_MS));
            if from >= to || to > i64::MAX as u64 || to - from > 366 * DAY_MS {
                anyhow::bail!("时间范围必须有效，且不能超过 366 天");
            }
            self.from_unix_ms = Some(from);
            self.to_unix_ms = Some(to);
        }
        if let Some(cursor) = &self.cursor
            && (!self.cursor_mode
                || cursor.timestamp_unix_ms > i64::MAX as u64
                || cursor.request_id.is_empty()
                || cursor.request_id.len() > MAX_LOG_STRING_BYTES)
        {
            anyhow::bail!("分页游标无效");
        }
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
            .is_some_and(|protocol| !matches!(protocol, "http" | "http_sse" | "ws"))
        {
            anyhow::bail!("协议筛选无效");
        }
        Ok(self)
    }
}

/// 从现有日志恢复最近一次具备有效生成计时的正式请求，不改变日志开关。
/// 日志库跨 Codex home 共用，归属仅通过精确会话 UUID 确定。
#[cfg(test)]
pub(crate) fn latest_session_recorded_speed(root: &Path, session_id: &str) -> Option<(f64, u64)> {
    let path = root.join(SQLITE_FILE_NAME);
    if !path.is_file() {
        return None;
    }
    let connection = open_query_connection(&path).ok()?;
    // 已有索引按会话和时间定位；旧库缺索引时不退化为周期性全库扫描。
    let sql = format!(
        "SELECT {OUTPUT_SPEED_SQL} AS speed, timestamp_unix_ms
        FROM route_request_logs INDEXED BY idx_route_request_logs_session_time
        WHERE codex_session_id IN (?1, ?2) AND request_kind = 'responses'
          AND COALESCE(codex_session_is_parent, 0) = 0
          AND ({OUTPUT_SPEED_SQL}) > 0
        ORDER BY timestamp_unix_ms DESC, request_id DESC LIMIT 1"
    );
    connection
        .query_row(
            &sql,
            params![session_id, format!("local:{session_id}")],
            |row| Ok((row.get::<_, f64>(0)?, row.get::<_, u64>(1)?)),
        )
        .ok()
        .filter(|(speed, _)| speed.is_finite() && *speed > 0.0)
}

/// 只统计保留的自身生成请求；缺失、采样或已清理的日志不补零，不宣称全会话覆盖。
#[cfg(test)]
pub(crate) fn session_recorded_timing(
    root: &Path,
    session_id: &str,
    boundary: Option<&crate::conversation_stats::RuntimeHistoryBoundary>,
) -> Option<SessionRecordedTiming> {
    let path = root.join(SQLITE_FILE_NAME);
    if !path.is_file() {
        return None;
    }
    let connection = open_query_connection(&path).ok()?;
    let (sql, values) = session_timing_query(session_id, boundary);
    connection
        .query_row(&sql, params_from_iter(values), |row| {
            Ok(SessionRecordedTiming {
                requests: row.get(0)?,
                model_duration_ms: row.get(1)?,
                average_ttft_ms: row.get(2)?,
                timing_request_count: row.get(3)?,
                ttft_sample_count: row.get(4)?,
            })
        })
        .ok()
        .filter(|value| value.requests > 0)
}

#[cfg(test)]
fn session_timing_query(
    session_id: &str,
    boundary: Option<&crate::conversation_stats::RuntimeHistoryBoundary>,
) -> (String, Vec<SqlValue>) {
    let mut sql = String::from("SELECT COUNT(*),
          SUM(CASE WHEN router_pre_upstream_ms >= 0 AND total_duration_ms >= router_pre_upstream_ms
              THEN total_duration_ms - router_pre_upstream_ms END),
          AVG(CASE WHEN downstream_first_content_ms >= 0 AND downstream_first_content_ms <= total_duration_ms
              THEN downstream_first_content_ms END),
          COUNT(CASE WHEN router_pre_upstream_ms >= 0 AND total_duration_ms >= router_pre_upstream_ms THEN 1 END),
          COUNT(CASE WHEN downstream_first_content_ms >= 0 AND downstream_first_content_ms <= total_duration_ms THEN 1 END)
         FROM route_request_logs INDEXED BY idx_route_request_logs_session_time
         WHERE codex_session_id IN (?, ?) AND request_kind = 'responses'
           AND COALESCE(codex_session_is_parent, 0) = 0 AND timestamp_unix_ms >= 0");
    let mut values = vec![
        SqlValue::Text(session_id.to_owned()),
        SqlValue::Text(format!("local:{session_id}")),
    ];
    append_runtime_history_boundary(&mut sql, &mut values, boundary);
    (sql, values)
}

/// 只读聚合历史与运行期边界之前的合格请求。短回复与零输出也参与累计，
/// 但缺 usage、缺计时、逆序计时和零生成时长不能进入分子或分母。
#[cfg(test)]
pub(crate) fn session_recorded_average(
    root: &Path,
    session_id: &str,
    boundary: Option<&crate::conversation_stats::RuntimeHistoryBoundary>,
) -> Option<SessionGenerationTotals> {
    let path = root.join(SQLITE_FILE_NAME);
    if !path.is_file() {
        return None;
    }
    let connection = open_query_connection(&path).ok()?;
    let (sql, values) = session_average_query(session_id, boundary);
    connection
        .query_row(&sql, params_from_iter(values), |row| {
            Ok(SessionGenerationTotals {
                output_tokens: row.get(0)?,
                generation_ms: row.get(1)?,
                requests: row.get(2)?,
            })
        })
        .ok()
        .filter(|totals| totals.speed().is_some())
}

#[cfg(test)]
fn session_average_query(
    session_id: &str,
    boundary: Option<&crate::conversation_stats::RuntimeHistoryBoundary>,
) -> (String, Vec<SqlValue>) {
    let mut sql = String::from(
        "SELECT SUM(output_tokens * 1.0),
        SUM((total_duration_ms - router_pre_upstream_ms - upstream_first_byte_ms) * 1.0), COUNT(*)
        FROM route_request_logs INDEXED BY idx_route_request_logs_session_time
        WHERE codex_session_id IN (?, ?) AND request_kind = 'responses'
          AND COALESCE(codex_session_is_parent, 0) = 0
          AND output_tokens >= 0 AND router_pre_upstream_ms >= 0
          AND upstream_first_byte_ms >= 0 AND total_duration_ms >= router_pre_upstream_ms
          AND total_duration_ms - router_pre_upstream_ms > upstream_first_byte_ms",
    );
    let mut values = vec![
        SqlValue::Text(session_id.to_owned()),
        SqlValue::Text(format!("local:{session_id}")),
    ];
    append_runtime_history_boundary(&mut sql, &mut values, boundary);
    (sql, values)
}

fn append_runtime_history_boundary(
    sql: &mut String,
    values: &mut Vec<SqlValue>,
    boundary: Option<&crate::conversation_stats::RuntimeHistoryBoundary>,
) {
    if let Some(boundary) = boundary {
        if boundary.exclude_entire_millisecond {
            sql.push_str(" AND timestamp_unix_ms < ?");
            values.push(SqlValue::Integer(to_i64(boundary.timestamp_unix_ms)));
        } else {
            // 显式上界供会话时间索引使用；仅边界同毫秒需要额外排除少量 ID。
            sql.push_str(" AND timestamp_unix_ms <= ?");
            values.push(SqlValue::Integer(to_i64(boundary.timestamp_unix_ms)));
            if !boundary.request_ids.is_empty() {
                sql.push_str(" AND (timestamp_unix_ms < ? OR request_id NOT IN (");
                values.push(SqlValue::Integer(to_i64(boundary.timestamp_unix_ms)));
                for (index, id) in boundary.request_ids.iter().enumerate() {
                    if index > 0 {
                        sql.push(',');
                    }
                    sql.push('?');
                    values.push(SqlValue::Text(id.clone()));
                }
                sql.push_str("))");
            }
        }
    }
}

/// 最近一轮只查询明确时间窗，沿用会话时间索引，不扫描其他会话或旧轮次。
pub(crate) fn last_turn_recorded_stats(
    root: &Path,
    session_id: &str,
    window: &crate::conversation_stats::TurnWindow,
    boundary: Option<&crate::conversation_stats::RuntimeHistoryBoundary>,
) -> Option<crate::conversation_stats::TurnRequestTotals> {
    let path = root.join(SQLITE_FILE_NAME);
    if !path.is_file() {
        return None;
    }
    let connection = open_query_connection(&path).ok()?;
    let (sql, values) = last_turn_stats_query(session_id, window, boundary);
    connection
        .query_row(&sql, params_from_iter(values), |row| {
            Ok(crate::conversation_stats::TurnRequestTotals {
                requests: row.get(0)?,
                model_duration_ms: row.get::<_, Option<u64>>(1)?.unwrap_or(0),
                model_samples: row.get(2)?,
                ttft_total_ms: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                ttft_samples: row.get(4)?,
                generation: SessionGenerationTotals {
                    output_tokens: row.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    generation_ms: row.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
                    requests: row.get(7)?,
                },
                excluded_boundary_requests: row.get(8)?,
            })
        })
        .ok()
        .filter(|totals| totals.requests > 0 || totals.excluded_boundary_requests > 0)
}

fn last_turn_stats_query(
    session_id: &str,
    window: &crate::conversation_stats::TurnWindow,
    boundary: Option<&crate::conversation_stats::RuntimeHistoryBoundary>,
) -> (String, Vec<SqlValue>) {
    let generation = "output_tokens >= 0 AND router_pre_upstream_ms >= 0 AND upstream_first_byte_ms >= 0 AND total_duration_ms >= router_pre_upstream_ms AND total_duration_ms - router_pre_upstream_ms > upstream_first_byte_ms";
    let model = "router_pre_upstream_ms >= 0 AND total_duration_ms >= router_pre_upstream_ms";
    let ttft =
        "downstream_first_content_ms >= 0 AND downstream_first_content_ms <= total_duration_ms";
    let mut sql = format!("SELECT COUNT(CASE WHEN inside_turn = 1 THEN 1 END),
        SUM(CASE WHEN {model} THEN total_duration_ms - router_pre_upstream_ms END),
        COUNT(CASE WHEN {model} THEN 1 END),
        SUM(CASE WHEN {ttft} THEN downstream_first_content_ms * 1.0 END),
        COUNT(CASE WHEN {ttft} THEN 1 END),
        SUM(CASE WHEN {generation} THEN output_tokens * 1.0 END),
        SUM(CASE WHEN {generation} THEN (total_duration_ms - router_pre_upstream_ms - upstream_first_byte_ms) * 1.0 END),
        COUNT(CASE WHEN {generation} THEN 1 END),
        COUNT(CASE WHEN inside_turn = 0 THEN 1 END)
        FROM (SELECT total_duration_ms, router_pre_upstream_ms, downstream_first_content_ms,
            upstream_first_byte_ms, output_tokens, 1 AS inside_turn
        FROM route_request_logs INDEXED BY idx_route_request_logs_session_time
        WHERE codex_session_id IN (?, ?) AND request_kind = 'responses'
        AND COALESCE(codex_session_is_parent, 0) = 0 AND timestamp_unix_ms > ?");
    let mut values = vec![
        SqlValue::Text(session_id.to_owned()),
        SqlValue::Text(format!("local:{session_id}")),
        SqlValue::Integer(to_i64(window.started_at_unix_ms)),
    ];
    if let Some(end) = window.completed_at_unix_ms {
        sql.push_str(" AND timestamp_unix_ms < ?");
        values.push(SqlValue::Integer(to_i64(end)));
    }
    append_runtime_history_boundary(&mut sql, &mut values, boundary);
    // 起止毫秒内没有 turn ID 的请求无法可靠归属。单独精确查这两个毫秒，
    // 不扩大上述历史范围，也不漏掉运行期去重上界之后的结束边界样本。
    sql.push_str(
        " UNION ALL SELECT NULL, NULL, NULL, NULL, NULL, 0
        FROM route_request_logs INDEXED BY idx_route_request_logs_session_time
        WHERE codex_session_id IN (?, ?) AND request_kind = 'responses'
        AND COALESCE(codex_session_is_parent, 0) = 0 AND timestamp_unix_ms IN (?",
    );
    values.extend([
        SqlValue::Text(session_id.to_owned()),
        SqlValue::Text(format!("local:{session_id}")),
        SqlValue::Integer(to_i64(window.started_at_unix_ms)),
    ]);
    if let Some(end) = window.completed_at_unix_ms {
        sql.push_str(", ?");
        values.push(SqlValue::Integer(to_i64(end)));
    }
    sql.push_str("))");
    (sql, values)
}

pub(crate) fn query_route_request_logs(
    root: &Path,
    backend: RouteRequestLogBackend,
    query: RouteRequestLogQuery,
) -> anyhow::Result<RouteRequestLogQueryPage> {
    if query.all_time || query.group_sort.is_some() || query.include_daily_trend {
        anyhow::bail!("全部历史、统计排序和每日趋势仅适用于用量统计");
    }
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
            next_cursor: None,
            has_more: false,
        });
    }

    let path = root.join(SQLITE_FILE_NAME);
    if !path.is_file() {
        return Ok(empty_query_page(query.page, query.page_size));
    }
    query_sqlite_route_request_logs(&path, &query)
}

fn query_sqlite_route_request_logs(
    path: &Path,
    query: &RouteRequestLogQuery,
) -> anyhow::Result<RouteRequestLogQueryPage> {
    let mut connection = open_query_connection(path)?;
    let transaction = connection.transaction()?;
    let optional_columns = sqlite_optional_columns(&transaction, path)?;
    let (mut where_clause, mut filter_params) =
        sqlite_query_filters(query, optional_columns.official_account);
    // Legacy page-number callers still receive a precise count. The UI uses cursors.
    let total = if query.cursor_mode {
        0
    } else {
        transaction.query_row(
            &format!("SELECT COUNT(*) FROM route_request_logs{where_clause}"),
            params_from_iter(filter_params.iter()),
            |row| row_u64(row, 0),
        )?
    };
    let total_pages = total.div_ceil(query.page_size);
    let offset = query.page.saturating_sub(1).saturating_mul(query.page_size);
    if let Some(cursor) = &query.cursor {
        where_clause.push_str(" AND (timestamp_unix_ms, request_id) < (?, ?)");
        filter_params.push(SqlValue::Integer(to_i64(cursor.timestamp_unix_ms)));
        filter_params.push(SqlValue::Text(cursor.request_id.clone()));
    }
    let pagination = if query.cursor_mode {
        "LIMIT ?"
    } else {
        "LIMIT ? OFFSET ?"
    };
    let tier_columns = if optional_columns.tiers {
        "requested_service_tier, service_tier"
    } else {
        "NULL, NULL"
    };
    let account_column = if optional_columns.official_account {
        "official_account_id"
    } else {
        "NULL"
    };
    let request_shape_columns = if optional_columns.request_shape {
        "request_input_state, request_input_items, request_has_previous_response_id,
            request_bytes, upstream_input_state, upstream_input_items,
            upstream_has_previous_response_id, upstream_bytes"
    } else {
        "NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL"
    };
    let response_header_column = if optional_columns.response_headers {
        "upstream_response_headers"
    } else {
        "NULL"
    };
    let response_model_column = if optional_columns.response_model {
        "upstream_response_model"
    } else {
        "NULL"
    };
    let concurrency_wait_column = if optional_columns.concurrency_wait {
        "concurrency_wait_ms"
    } else {
        "NULL"
    };
    let select_sql = format!(
        "SELECT
            request_id, trace_id, timestamp_unix_ms, provider, provider_name,
            requested_model, model, reasoning_effort, thinking_budget_tokens,
            ttft_ms, router_pre_upstream_ms, upstream_first_byte_ms,
            downstream_first_content_ms, upstream_header_ms, total_duration_ms, queue_delay_ms,
            input_tokens, output_tokens, cached_input_tokens,
            cache_creation_input_tokens, reasoning_output_tokens, total_tokens,
            usage_reported, usage_unavailable_reason, request_protocol,
            upstream_transport, request_kind, status, status_code,
            upstream_status_code, error_code, completion_reason, fallback_count,
            fallback_reason, upstream_authority,
            upstream_request_id, upstream_protocol, protocol_bridge,
            first_byte_source, subagent, upstream_error_summary,
            codex_session_id, codex_session_is_parent, {tier_columns},
            upstream_request_headers, {account_column}, {request_shape_columns},
            {response_header_column}, {response_model_column},
            {OUTPUT_SPEED_SQL} AS output_tokens_per_second, {concurrency_wait_column},
            {diagnostic_columns}
         FROM route_request_logs{where_clause}
         ORDER BY timestamp_unix_ms DESC, request_id DESC
         {pagination}",
        diagnostic_columns = if optional_columns.diagnostics {
            "route_decision, upstream_attempts, attempts_omitted"
        } else {
            "NULL, NULL, NULL"
        }
    );
    let mut page_params = filter_params;
    page_params.push(SqlValue::Integer(to_i64(
        query.page_size + u64::from(query.cursor_mode),
    )));
    if !query.cursor_mode {
        page_params.push(SqlValue::Integer(to_i64(offset)));
    }
    let mut statement = transaction.prepare(&select_sql)?;
    let mut items = statement
        .query_map(params_from_iter(page_params), query_item_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    transaction.commit()?;
    let has_more = if query.cursor_mode {
        items.len() > query.page_size as usize
    } else {
        query.page < total_pages
    };
    items.truncate(query.page_size as usize);
    let next_cursor = if has_more {
        items.last().map(|item| RouteRequestLogCursor {
            timestamp_unix_ms: item.timestamp_unix_ms,
            request_id: item.request_id.clone(),
        })
    } else {
        None
    };

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
        next_cursor,
        has_more,
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
        next_cursor: None,
        has_more: false,
    }
}

/// `has_official_account_column` keeps queries working against a database the
/// writer has not migrated yet; the account filter and search fall back to the
/// columns every version ships.
fn sqlite_query_filters(
    query: &RouteRequestLogQuery,
    has_official_account_column: bool,
) -> (String, Vec<SqlValue>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut values = Vec::new();
    if let (Some(from), Some(to)) = (query.from_unix_ms, query.to_unix_ms) {
        clauses.push("timestamp_unix_ms >= ? AND timestamp_unix_ms < ?".into());
        values.extend([
            SqlValue::Integer(to_i64(from)),
            SqlValue::Integer(to_i64(to)),
        ]);
    }
    for (column, value) in [
        ("request_id", &query.request_id),
        ("request_kind", &query.request_kind),
        ("codex_session_id", &query.session_id),
    ] {
        if let Some(value) = value {
            clauses.push(format!("{column} = ?"));
            values.push(SqlValue::Text(value.clone()));
        }
    }
    if let Some(search) = &query.search {
        let pattern = format!("%{}%", escape_like_pattern(search));
        let account_search = if has_official_account_column {
            "
              OR COALESCE(official_account_id, '') LIKE ? ESCAPE '\\'"
        } else {
            ""
        };
        let search_clause = format!(
            "(request_id LIKE ? ESCAPE '\\'
              OR trace_id LIKE ? ESCAPE '\\'
              OR COALESCE(provider, '') LIKE ? ESCAPE '\\'
              OR COALESCE(provider_name, '') LIKE ? ESCAPE '\\'
              OR requested_model LIKE ? ESCAPE '\\'
              OR COALESCE(model, '') LIKE ? ESCAPE '\\'
              OR COALESCE(error_code, '') LIKE ? ESCAPE '\\'
              OR COALESCE(upstream_request_id, '') LIKE ? ESCAPE '\\'
              OR COALESCE(upstream_error_summary, '') LIKE ? ESCAPE '\\'
              OR COALESCE(codex_session_id, '') LIKE ? ESCAPE '\\'{account_search})",
        );
        values.extend(
            (0..if has_official_account_column { 11 } else { 10 })
                .map(|_| SqlValue::Text(pattern.clone())),
        );
        clauses.push(search_clause);
    }
    if let Some(provider) = &query.provider {
        if query.cursor_mode {
            clauses.push("provider = ? COLLATE NOCASE".into());
        } else {
            clauses
                .push("(provider = ? COLLATE NOCASE OR provider_name = ? COLLATE NOCASE)".into());
            values.push(SqlValue::Text(provider.clone()));
        }
        values.push(SqlValue::Text(provider.clone()));
    }
    if let Some(model) = &query.model {
        if query.cursor_mode {
            clauses.push("COALESCE(model, requested_model) = ? COLLATE NOCASE".into());
        } else {
            clauses.push("(model = ? COLLATE NOCASE OR requested_model = ? COLLATE NOCASE)".into());
            values.push(SqlValue::Text(model.clone()));
        }
        values.push(SqlValue::Text(model.clone()));
    }
    if let Some(status) = &query.status {
        clauses.push("status = ?".into());
        values.push(SqlValue::Text(status.clone()));
    }
    if let Some(protocol) = &query.protocol {
        clauses.push("upstream_transport = ?".into());
        values.push(SqlValue::Text(protocol.clone()));
    }
    if let Some(account_id) = &query.official_account_id
        && has_official_account_column
    {
        clauses.push("official_account_id = ?".into());
        values.push(SqlValue::Text(account_id.clone()));
    }
    if clauses.is_empty() {
        (String::new(), values)
    } else {
        (format!(" WHERE {}", clauses.join(" AND ")), values)
    }
}

fn summary_columns() -> String {
    format!(
        "COUNT(*),
    COALESCE(SUM(status = 'succeeded'), 0), COALESCE(SUM(status = 'failed'), 0),
    COALESCE(SUM(status = 'incomplete'), 0), COALESCE(SUM(status = 'cancelled'), 0),
    AVG(CASE WHEN total_duration_ms >= 0 THEN total_duration_ms END),
    AVG(CASE WHEN COALESCE(downstream_first_content_ms, ttft_ms) >= 0
        THEN COALESCE(downstream_first_content_ms, ttft_ms) END),
    {OUTPUT_SPEED_SUM_SQL},
    AVG(CASE WHEN router_pre_upstream_ms >= 0 THEN router_pre_upstream_ms END),
    AVG(CASE WHEN upstream_header_ms >= 0 THEN upstream_header_ms END),
    AVG(CASE WHEN upstream_first_byte_ms >= 0 THEN upstream_first_byte_ms END),
    AVG(CASE WHEN downstream_first_content_ms >= 0 THEN downstream_first_content_ms END),
    AVG(CASE WHEN queue_delay_ms >= 0 THEN queue_delay_ms END),
    SUM(input_tokens), SUM(output_tokens), SUM(total_tokens), SUM(cached_input_tokens),
    COALESCE(SUM(usage_reported != 0), 0), COUNT(total_tokens)"
    )
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RouteRequestLogSummary> {
    let total = row_u64(row, 0)?;
    let succeeded_count = row_u64(row, 1)?;
    Ok(RouteRequestLogSummary {
        total,
        succeeded_count,
        failed_count: row_u64(row, 2)?,
        incomplete_count: row_u64(row, 3)?,
        cancelled_count: row_u64(row, 4)?,
        avg_duration: row.get(5)?,
        avg_ttft: row.get(6)?,
        avg_output_tokens_per_second: row.get(7)?,
        avg_router_pre_upstream: row.get(8)?,
        avg_upstream_header: row.get(9)?,
        avg_upstream_first_byte: row.get(10)?,
        avg_downstream_first_content: row.get(11)?,
        avg_queue_delay: row.get(12)?,
        success_rate: (total > 0).then(|| succeeded_count as f64 * 100.0 / total as f64),
        input_tokens_sum: row_optional_u64(row, 13)?,
        output_tokens_sum: row_optional_u64(row, 14)?,
        total_tokens_sum: row_optional_u64(row, 15)?,
        cached_tokens_sum: row_optional_u64(row, 16)?,
        usage_reported_count: row_u64(row, 17)?,
        total_tokens_known_count: row_u64(row, 18)?,
    })
}

pub(crate) fn query_route_request_log_models(
    root: &Path,
    backend: RouteRequestLogBackend,
    query: RouteRequestLogModelQuery,
) -> anyhow::Result<RouteRequestLogModelPage> {
    let after_model = query.after_model;
    anyhow::ensure!(
        after_model.as_ref().is_none_or(|value| value.len() <= 4096),
        "模型游标过长"
    );
    let query = RouteRequestLogQuery {
        from_unix_ms: query.from_unix_ms,
        to_unix_ms: query.to_unix_ms,
        provider: query.provider,
        official_account_id: query.official_account_id,
        cursor_mode: true,
        ..Default::default()
    }
    .normalize()?;
    let mut result = RouteRequestLogModelPage {
        queryable: backend == RouteRequestLogBackend::Sqlite,
        models: Vec::new(),
        next_cursor: None,
    };
    let path = root.join(SQLITE_FILE_NAME);
    if !result.queryable || !path.is_file() {
        return Ok(result);
    }
    let mut connection = open_query_connection(&path)?;
    let transaction = connection.transaction()?;
    let columns = sqlite_optional_columns(&transaction, &path)?;
    let (mut filter, mut values) = sqlite_query_filters(&query, columns.official_account);
    if query.official_account_id.is_some() && !columns.official_account {
        return Ok(result);
    }
    filter.push_str(if filter.is_empty() {
        " WHERE "
    } else {
        " AND "
    });
    filter.push_str("TRIM(COALESCE(model, requested_model)) <> ''");
    if let Some(after) = after_model {
        filter.push_str(" AND COALESCE(model, requested_model) COLLATE BINARY > ?");
        values.push(SqlValue::Text(after));
    }
    result.models = transaction.prepare(&format!(
        "SELECT DISTINCT COALESCE(model, requested_model) AS model_key FROM route_request_logs{filter} ORDER BY model_key COLLATE BINARY LIMIT 201"
    ))?.query_map(params_from_iter(values.iter()), |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    if result.models.len() > 200 {
        result.models.truncate(200);
        result.next_cursor = result.models.last().cloned();
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) fn query_route_request_log_stats(
    root: &Path,
    backend: RouteRequestLogBackend,
    query: RouteRequestLogQuery,
) -> anyhow::Result<RouteRequestLogAnalytics> {
    query_route_request_log_stats_with_model_aliases(root, backend, query, Default::default())
}

pub(crate) fn query_route_request_log_stats_with_model_aliases(
    root: &Path,
    backend: RouteRequestLogBackend,
    mut query: RouteRequestLogQuery,
    model_aliases: RouteRequestLogModelAliases,
) -> anyhow::Result<RouteRequestLogAnalytics> {
    if query.all_time
        && (query.from_unix_ms.is_some()
            || query.to_unix_ms.is_some()
            || query.cursor_mode
            || query.cursor.is_some())
    {
        anyhow::bail!("全部历史统计不能同时指定时间范围或分页游标");
    }
    query.cursor_mode = !query.all_time;
    query.cursor = None;
    let mut query = query.normalize()?;
    let model_column = if model_aliases.sources.is_empty() {
        "COALESCE(model, requested_model)"
    } else {
        "route_request_log_model(model, requested_model)"
    };
    let to = query.to_unix_ms.unwrap_or_else(unix_timestamp_ms);
    let from = query
        .from_unix_ms
        .unwrap_or_else(|| to.saturating_sub(DAY_MS));
    let queryable = backend == RouteRequestLogBackend::Sqlite;
    let bucket_ms = if to - from <= 7 * DAY_MS {
        DAY_MS / 24
    } else {
        DAY_MS
    };
    let mut result = RouteRequestLogAnalytics {
        status: if queryable { "ok" } else { "unavailable" },
        backend: if queryable { "sqlite" } else { "ndjson" },
        queryable,
        reason: (!queryable).then_some("ndjson_not_queryable"),
        from_unix_ms: from,
        to_unix_ms: to,
        summary: RouteRequestLogSummary::default(),
        groups: Vec::new(),
        groups_truncated: false,
        trend: Vec::new(),
        daily_trend: Vec::new(),
        bucket_ms,
        database_bytes: fs::metadata(root.join(SQLITE_FILE_NAME))
            .map_or(0, |metadata| metadata.len()),
        wal_bytes: fs::metadata(root.join(format!("{SQLITE_FILE_NAME}-wal")))
            .map_or(0, |metadata| metadata.len()),
    };
    let path = root.join(SQLITE_FILE_NAME);
    if !queryable || !path.is_file() {
        return Ok(result);
    }
    let mut connection = open_query_connection(&path)?;
    if !model_aliases.sources.is_empty() {
        model_aliases.register(&connection)?;
    }
    let transaction = connection.transaction()?;
    let optional_columns = sqlite_optional_columns(&transaction, &path)?;
    if query.all_time {
        // Use the same filters and snapshot as the aggregates, excluding future timestamps.
        query.cursor_mode = true;
        query.from_unix_ms = Some(0);
        query.to_unix_ms = Some(to);
        let (where_clause, values) =
            sqlite_query_filters(&query, optional_columns.official_account);
        let earliest: Option<u64> = transaction.query_row(
            &format!("SELECT MIN(timestamp_unix_ms) FROM route_request_logs{where_clause}"),
            params_from_iter(values.iter()),
            |row| row_optional_u64(row, 0),
        )?;
        result.from_unix_ms = earliest.unwrap_or(from);
        query.from_unix_ms = Some(result.from_unix_ms);
        let span = to - result.from_unix_ms;
        result.bucket_ms = if span <= 7 * DAY_MS {
            DAY_MS / 24
        } else {
            span.div_ceil(366 * DAY_MS).max(1) * DAY_MS
        };
    }
    let (where_clause, values) = sqlite_query_filters(&query, optional_columns.official_account);
    let summary_columns = summary_columns();
    result.summary = transaction.query_row(
        &format!("SELECT {summary_columns} FROM route_request_logs{where_clause}"),
        params_from_iter(values.iter()),
        summary_from_row,
    )?;
    if let Some(group_by) = &query.group_by
        && (group_by != "official_account" || optional_columns.official_account)
    {
        let column = match group_by.as_str() {
            "model" => model_column,
            "provider" => "provider",
            "status" => "status",
            "protocol" => "upstream_transport",
            "request_kind" => "request_kind",
            "session" => "codex_session_id",
            "official_account" => "official_account_id",
            _ => unreachable!("validated grouping"),
        };
        let order = if query.group_sort.as_deref() == Some("tokens") {
            "SUM(total_tokens) IS NULL, SUM(total_tokens) DESC, group_key"
        } else {
            "COUNT(*) DESC, group_key"
        };
        let sql = format!(
            "SELECT {summary_columns}, COALESCE({column}, '') AS group_key
            FROM route_request_logs{where_clause} GROUP BY group_key
            ORDER BY {order} LIMIT 51"
        );
        result.groups = transaction
            .prepare(&sql)?
            .query_map(params_from_iter(values.iter()), |row| {
                Ok(RouteRequestLogGroup {
                    key: row.get(19)?,
                    summary: summary_from_row(row)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        result.groups_truncated = result.groups.len() > 50;
        result.groups.truncate(50);
    }
    // The range and adaptive UTC bucket width produce at most 367 buckets.
    let bucket_ms = result.bucket_ms;
    let sql = format!("SELECT timestamp_unix_ms / {bucket_ms} * {bucket_ms} AS bucket,
        COUNT(*), SUM(total_tokens), AVG(CASE WHEN total_duration_ms >= 0 THEN total_duration_ms END),
        AVG(CASE WHEN COALESCE(downstream_first_content_ms, ttft_ms) >= 0 THEN COALESCE(downstream_first_content_ms, ttft_ms) END),
        {OUTPUT_SPEED_SUM_SQL},
        AVG(CASE WHEN downstream_first_content_ms >= 0 THEN downstream_first_content_ms END)
        FROM route_request_logs{where_clause} GROUP BY bucket ORDER BY bucket");
    result.trend = transaction
        .prepare(&sql)?
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(RouteRequestLogTrend {
                timestamp_unix_ms: row_u64(row, 0)?,
                total: row_u64(row, 1)?,
                total_tokens_sum: row_optional_u64(row, 2)?,
                avg_duration: row.get(3)?,
                avg_ttft: row.get(4)?,
                avg_output_tokens_per_second: row.get(5)?,
                avg_downstream_first_content: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if query.include_daily_trend {
        let sql = format!(
            "SELECT timestamp_unix_ms / {DAY_MS} * {DAY_MS} AS day,
            COUNT(*), SUM(total_tokens), COUNT(total_tokens)
            FROM route_request_logs{where_clause} GROUP BY day ORDER BY day"
        );
        result.daily_trend = transaction
            .prepare(&sql)?
            .query_map(params_from_iter(values.iter()), |row| {
                Ok(RouteRequestLogDailyTrend {
                    timestamp_unix_ms: row_u64(row, 0)?,
                    total: row_u64(row, 1)?,
                    total_tokens_sum: row_optional_u64(row, 2)?,
                    total_tokens_known_count: row_u64(row, 3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }
    transaction.commit()?;
    Ok(result)
}

#[derive(Clone, Copy)]
struct RouteRequestLogOptionalColumns {
    diagnostics: bool,
    /// Billing tier columns used by the pricing estimate.
    tiers: bool,
    /// The per-route official account id.
    official_account: bool,
    /// Request body shape summary columns written for every request.
    request_shape: bool,
    /// 上游响应头列随 schema 版本 11 加入，旧库尚未迁移时按缺失处理。
    response_headers: bool,
    /// 上游回报的实际使用模型列随 schema 版本 12 加入。
    response_model: bool,
    concurrency_wait: bool,
}

/// These columns are added by the writer at open time and never dropped, so a
/// positive probe can be remembered per database path. Negative results keep
/// probing: the writer may add the columns while this process is running.
fn sqlite_optional_columns(
    connection: &Connection,
    path: &Path,
) -> rusqlite::Result<RouteRequestLogOptionalColumns> {
    static TIERED_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    static ACCOUNTED_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    static SHAPED_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    static RESPONSE_HEADER_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    static RESPONSE_MODEL_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    static CONCURRENCY_WAIT_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    static DIAGNOSTIC_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let tiers_known = sqlite_path_known(&TIERED_PATHS, path);
    let official_known = sqlite_path_known(&ACCOUNTED_PATHS, path);
    let shape_known = sqlite_path_known(&SHAPED_PATHS, path);
    let headers_known = sqlite_path_known(&RESPONSE_HEADER_PATHS, path);
    let model_known = sqlite_path_known(&RESPONSE_MODEL_PATHS, path);
    let wait_known = sqlite_path_known(&CONCURRENCY_WAIT_PATHS, path);
    let diagnostics_known = sqlite_path_known(&DIAGNOSTIC_PATHS, path);
    if tiers_known
        && official_known
        && shape_known
        && headers_known
        && model_known
        && wait_known
        && diagnostics_known
    {
        return Ok(RouteRequestLogOptionalColumns {
            diagnostics: true,
            tiers: true,
            official_account: true,
            request_shape: true,
            response_headers: true,
            response_model: true,
            concurrency_wait: true,
        });
    }
    let columns = table_columns(connection, "route_request_logs")?;
    Ok(RouteRequestLogOptionalColumns {
        diagnostics: sqlite_remember_column(
            &DIAGNOSTIC_PATHS,
            path,
            ["route_decision", "upstream_attempts", "attempts_omitted"]
                .iter()
                .all(|column| columns.contains(*column)),
        ),
        concurrency_wait: sqlite_remember_column(
            &CONCURRENCY_WAIT_PATHS,
            path,
            columns.contains("concurrency_wait_ms"),
        ),
        tiers: sqlite_remember_column(&TIERED_PATHS, path, columns.contains("service_tier")),
        official_account: sqlite_remember_column(
            &ACCOUNTED_PATHS,
            path,
            columns.contains("official_account_id"),
        ),
        request_shape: sqlite_remember_column(
            &SHAPED_PATHS,
            path,
            columns.contains("request_bytes"),
        ),
        response_headers: sqlite_remember_column(
            &RESPONSE_HEADER_PATHS,
            path,
            columns.contains("upstream_response_headers"),
        ),
        response_model: sqlite_remember_column(
            &RESPONSE_MODEL_PATHS,
            path,
            columns.contains("upstream_response_model"),
        ),
    })
}

fn sqlite_path_known(known_paths: &OnceLock<Mutex<HashSet<PathBuf>>>, path: &Path) -> bool {
    let known = known_paths.get_or_init(|| Mutex::new(HashSet::new()));
    lock_unpoisoned(known).contains(path)
}

fn sqlite_remember_column(
    known_paths: &OnceLock<Mutex<HashSet<PathBuf>>>,
    path: &Path,
    exists: bool,
) -> bool {
    if exists {
        let known = known_paths.get_or_init(|| Mutex::new(HashSet::new()));
        lock_unpoisoned(known).insert(path.to_path_buf());
    }
    exists
}

fn open_query_connection(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "query_only", "ON")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    connection.progress_handler(10_000, Some(move || Instant::now() >= deadline));
    Ok(connection)
}

fn query_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RouteRequestLogQueryItem> {
    Ok(RouteRequestLogQueryItem {
        route_decision: row_json(row, 59)?,
        upstream_attempts: row_json(row, 60)?.unwrap_or_default(),
        attempts_omitted: row.get::<_, Option<u32>>(61)?.unwrap_or_default(),
        requested_service_tier: row.get(43)?,
        service_tier: row.get(44)?,
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
        router_pre_upstream_ms: row_optional_u64(row, 10)?,
        upstream_first_byte_ms: row_optional_u64(row, 11)?,
        downstream_first_content_ms: row_optional_u64(row, 12)?,
        upstream_header_ms: row_optional_u64(row, 13)?,
        total_duration_ms: row_u64(row, 14)?,
        queue_delay_ms: row_u64(row, 15)?,
        concurrency_wait_ms: row_optional_u64(row, 58)?,
        input_tokens: row_optional_u64(row, 16)?,
        output_tokens: row_optional_u64(row, 17)?,
        cached_input_tokens: row_optional_u64(row, 18)?,
        cache_creation_input_tokens: row_optional_u64(row, 19)?,
        reasoning_output_tokens: row_optional_u64(row, 20)?,
        total_tokens: row_optional_u64(row, 21)?,
        usage_reported: row.get(22)?,
        usage_unavailable_reason: row.get(23)?,
        request_protocol: row.get(24)?,
        upstream_transport: row.get(25)?,
        request_kind: row.get(26)?,
        status: row.get(27)?,
        status_code: row_optional_u16(row, 28)?,
        upstream_status_code: row_optional_u16(row, 29)?,
        error_code: row.get(30)?,
        completion_reason: row.get(31)?,
        fallback_count: row_u32(row, 32)?,
        fallback_reason: row.get(33)?,
        upstream_authority: row.get(34)?,
        upstream_request_id: row.get(35)?,
        upstream_protocol: row.get(36)?,
        protocol_bridge: row.get(37)?,
        first_byte_source: row.get(38)?,
        subagent: row.get(39)?,
        upstream_error_summary: row.get(40)?,
        codex_session_id: row.get(41)?,
        codex_session_is_parent: row.get(42)?,
        upstream_request_headers: row.get(45)?,
        official_account_id: row.get(46)?,
        request_input_state: row.get(47)?,
        request_input_items: row_optional_u64(row, 48)?,
        request_has_previous_response_id: row.get(49)?,
        request_bytes: row_optional_u64(row, 50)?,
        upstream_input_state: row.get(51)?,
        upstream_input_items: row_optional_u64(row, 52)?,
        upstream_has_previous_response_id: row.get(53)?,
        upstream_bytes: row_optional_u64(row, 54)?,
        upstream_response_headers: row.get(55)?,
        upstream_response_model: row.get(56)?,
        output_tokens_per_second: row.get(57)?,
    })
}

fn row_json<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    let Some(value) = row.get::<_, Option<String>>(index)? else {
        return Ok(None);
    };
    if value.len() > 16 * 1024 {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            std::io::Error::new(std::io::ErrorKind::InvalidData, "路由诊断超过大小上限").into(),
        ));
    }
    serde_json::from_str(&value).map(Some).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, error.into())
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

pub(crate) fn clear_route_request_log_files(
    root: &Path,
    recording_enabled: bool,
) -> RouteRequestLogClearResult {
    let mut result = RouteRequestLogClearResult::empty(recording_enabled);
    let mut targets = vec![
        OsString::from(SQLITE_FILE_NAME),
        OsString::from(format!("{SQLITE_FILE_NAME}-wal")),
        OsString::from(format!("{SQLITE_FILE_NAME}-shm")),
        OsString::from(format!("{SQLITE_FILE_NAME}-journal")),
        OsString::from(NDJSON_FILE_NAME),
    ];

    match fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) if is_ndjson_rotated_file_name(&entry.file_name()) => {
                        targets.push(entry.file_name());
                    }
                    Ok(_) => {}
                    Err(error) => append_clear_error(
                        &mut result.error,
                        format!("读取请求日志目录项失败：{error}"),
                    ),
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            append_clear_error(&mut result.error, format!("读取请求日志目录失败：{error}"))
        }
    }

    targets.sort();
    targets.dedup();
    for file_name in targets {
        let path = root.join(&file_name);
        match fs::remove_file(&path) {
            Ok(()) => {
                result.removed_file_count += 1;
                result
                    .removed_files
                    .push(file_name.to_string_lossy().into_owned());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => append_clear_error(
                &mut result.error,
                format!(
                    "删除请求日志文件 {} 失败：{error}",
                    file_name.to_string_lossy()
                ),
            ),
        }
    }
    result.refresh_status();
    result
}

fn is_ndjson_rotated_file_name(file_name: &OsStr) -> bool {
    let Some(file_name) = file_name.to_str() else {
        return false;
    };
    let Some(index) = file_name.strip_prefix(&format!("{NDJSON_FILE_NAME}.")) else {
        return false;
    };
    !index.is_empty() && !index.starts_with('0') && index.as_bytes().iter().all(u8::is_ascii_digit)
}

fn append_clear_error(target: &mut Option<String>, error: String) {
    if let Some(current) = target {
        current.push('；');
        current.push_str(&error);
    } else {
        *target = Some(error);
    }
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
