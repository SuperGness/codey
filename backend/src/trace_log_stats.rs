use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Serialize, Serializer};

use crate::trace_log_guard;

const RECENT_DAYS: u8 = 7;
const TOP_TARGETS: usize = 8;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceLogStatsSnapshot {
    pub pending: bool,
    pub captured_at: u64,
    pub recent_days_window: u8,
    pub databases_found: usize,
    pub databases_scanned: usize,
    pub database_bytes: u64,
    pub row_count: u64,
    pub estimated_log_bytes: u64,
    pub recent_row_count: u64,
    pub recent_estimated_bytes: u64,
    pub oldest_timestamp: Option<i64>,
    pub newest_timestamp: Option<i64>,
    pub daily: Vec<TraceLogDailyStats>,
    pub levels: Vec<TraceLogGroupStats>,
    pub top_targets: Vec<TraceLogGroupStats>,
    pub errors: Vec<String>,
}

impl TraceLogStatsSnapshot {
    pub fn idle() -> Self {
        Self {
            recent_days_window: RECENT_DAYS,
            ..Self::default()
        }
    }

    pub fn pending() -> Self {
        Self {
            pending: true,
            captured_at: timestamp_seconds(),
            recent_days_window: RECENT_DAYS,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct TraceLogStatsHandle {
    snapshot: Arc<RwLock<TraceLogStatsSnapshot>>,
}

impl TraceLogStatsHandle {
    pub fn idle() -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(TraceLogStatsSnapshot::idle())),
        }
    }

    pub fn begin_refresh(&self) -> bool {
        let mut current = match self.snapshot.write() {
            Ok(current) => current,
            Err(poisoned) => poisoned.into_inner(),
        };
        if current.pending {
            return false;
        }
        *current = TraceLogStatsSnapshot::pending();
        true
    }

    pub fn replace(&self, snapshot: TraceLogStatsSnapshot) {
        match self.snapshot.write() {
            Ok(mut current) => *current = snapshot,
            Err(poisoned) => *poisoned.into_inner() = snapshot,
        }
    }
}

impl Default for TraceLogStatsHandle {
    fn default() -> Self {
        Self::idle()
    }
}

impl Serialize for TraceLogStatsHandle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.snapshot.read() {
            Ok(snapshot) => snapshot.serialize(serializer),
            Err(poisoned) => poisoned.into_inner().serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceLogDailyStats {
    pub date: String,
    pub rows: u64,
    pub estimated_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceLogGroupStats {
    pub name: String,
    pub rows: u64,
    pub estimated_bytes: u64,
}

pub fn snapshot(home: &Path) -> TraceLogStatsSnapshot {
    let mut snapshot = TraceLogStatsSnapshot {
        captured_at: timestamp_seconds(),
        recent_days_window: RECENT_DAYS,
        ..TraceLogStatsSnapshot::default()
    };
    let paths = match trace_log_guard::log_database_paths(home) {
        Ok(paths) => paths,
        Err(error) => {
            snapshot.errors.push(error.to_string());
            return snapshot;
        }
    };

    let mut daily = BTreeMap::<String, TraceLogDailyStats>::new();
    let mut levels = BTreeMap::<String, TraceLogGroupStats>::new();
    let mut targets = BTreeMap::<String, TraceLogGroupStats>::new();
    for path in paths {
        snapshot.databases_found += 1;
        snapshot.database_bytes = snapshot
            .database_bytes
            .saturating_add(trace_log_guard::database_family_bytes(&path));
        match read_database(&path) {
            Ok(Some(database)) => {
                snapshot.databases_scanned += 1;
                snapshot.row_count = snapshot.row_count.saturating_add(database.row_count);
                snapshot.estimated_log_bytes = snapshot
                    .estimated_log_bytes
                    .saturating_add(database.estimated_log_bytes);
                snapshot.oldest_timestamp =
                    min_timestamp(snapshot.oldest_timestamp, database.oldest_timestamp);
                snapshot.newest_timestamp =
                    max_timestamp(snapshot.newest_timestamp, database.newest_timestamp);
                merge_daily(&mut daily, database.daily);
                merge_groups(&mut levels, database.levels);
                merge_groups(&mut targets, database.targets);
            }
            Ok(None) => {}
            Err(error) => snapshot.errors.push(format!(
                "{}: {error:#}",
                path.file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default()
            )),
        }
    }

    snapshot.daily = daily.into_values().collect();
    snapshot.recent_row_count = snapshot.daily.iter().map(|item| item.rows).sum();
    snapshot.recent_estimated_bytes = snapshot.daily.iter().map(|item| item.estimated_bytes).sum();
    snapshot.levels = sorted_groups(levels, usize::MAX);
    snapshot.top_targets = sorted_groups(targets, TOP_TARGETS);
    snapshot
}

#[derive(Debug, Default)]
struct DatabaseStats {
    row_count: u64,
    estimated_log_bytes: u64,
    oldest_timestamp: Option<i64>,
    newest_timestamp: Option<i64>,
    daily: Vec<TraceLogDailyStats>,
    levels: Vec<TraceLogGroupStats>,
    targets: Vec<TraceLogGroupStats>,
}

fn read_database(path: &Path) -> Result<Option<DatabaseStats>> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("打开日志库失败：{}", path.display()))?;
    connection.execute_batch("PRAGMA query_only=ON; PRAGMA temp_store=MEMORY;")?;
    let table_found = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='logs' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !table_found {
        return Ok(None);
    }

    let columns = table_columns(&connection)?;
    let estimate_expression = if columns.contains("estimated_bytes") {
        "CASE WHEN estimated_bytes > 0 THEN estimated_bytes ELSE 0 END"
    } else if columns.contains("feedback_log_body") {
        "length(COALESCE(feedback_log_body, ''))"
    } else {
        "0"
    };
    let timestamp_expressions = if columns.contains("ts") {
        ("MIN(ts)", "MAX(ts)")
    } else {
        ("NULL", "NULL")
    };
    let summary_sql = format!(
        "SELECT COUNT(*), COALESCE(SUM({estimate_expression}), 0), {}, {} FROM logs",
        timestamp_expressions.0, timestamp_expressions.1
    );
    let (rows, estimated_bytes, oldest_timestamp, newest_timestamp) =
        connection.query_row(&summary_sql, [], |row| {
            Ok((
                nonnegative(row.get::<_, i64>(0)?),
                nonnegative(row.get::<_, i64>(1)?),
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?;

    let daily = if columns.contains("ts") {
        query_daily(&connection, estimate_expression)?
    } else {
        Vec::new()
    };
    let level_expression = if columns.contains("level") {
        "COALESCE(NULLIF(TRIM(level), ''), 'UNKNOWN')"
    } else {
        "'UNKNOWN'"
    };
    let target_expression = if columns.contains("target") {
        "COALESCE(NULLIF(TRIM(target), ''), 'UNKNOWN')"
    } else {
        "'UNKNOWN'"
    };

    Ok(Some(DatabaseStats {
        row_count: rows,
        estimated_log_bytes: estimated_bytes,
        oldest_timestamp,
        newest_timestamp,
        daily,
        levels: query_groups(&connection, level_expression, estimate_expression)?,
        targets: query_groups(&connection, target_expression, estimate_expression)?,
    }))
}

fn table_columns(connection: &Connection) -> Result<HashSet<String>> {
    let mut statement = connection.prepare("PRAGMA table_info(logs)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(columns)
}

fn query_daily(
    connection: &Connection,
    estimate_expression: &str,
) -> Result<Vec<TraceLogDailyStats>> {
    let sql = format!(
        "SELECT date(ts, 'unixepoch', 'localtime') AS day,
                COUNT(*), COALESCE(SUM({estimate_expression}), 0)
         FROM logs
         WHERE ts >= CAST(strftime('%s', 'now', 'start of day', '-6 days') AS INTEGER)
         GROUP BY day
         ORDER BY day"
    );
    let mut statement = connection.prepare(&sql)?;
    let values = statement
        .query_map([], |row| {
            Ok(TraceLogDailyStats {
                date: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                rows: nonnegative(row.get::<_, i64>(1)?),
                estimated_bytes: nonnegative(row.get::<_, i64>(2)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values
        .into_iter()
        .filter(|value| !value.date.is_empty())
        .collect())
}

fn query_groups(
    connection: &Connection,
    group_expression: &str,
    estimate_expression: &str,
) -> Result<Vec<TraceLogGroupStats>> {
    let sql = format!(
        "SELECT {group_expression} AS group_name,
                COUNT(*), COALESCE(SUM({estimate_expression}), 0)
         FROM logs
         GROUP BY group_name"
    );
    let mut statement = connection.prepare(&sql)?;
    let values = statement
        .query_map([], |row| {
            Ok(TraceLogGroupStats {
                name: row.get::<_, String>(0)?,
                rows: nonnegative(row.get::<_, i64>(1)?),
                estimated_bytes: nonnegative(row.get::<_, i64>(2)?),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}

fn merge_daily(
    destination: &mut BTreeMap<String, TraceLogDailyStats>,
    values: Vec<TraceLogDailyStats>,
) {
    for value in values {
        let entry = destination
            .entry(value.date.clone())
            .or_insert_with(|| TraceLogDailyStats {
                date: value.date,
                ..TraceLogDailyStats::default()
            });
        entry.rows = entry.rows.saturating_add(value.rows);
        entry.estimated_bytes = entry.estimated_bytes.saturating_add(value.estimated_bytes);
    }
}

fn merge_groups(
    destination: &mut BTreeMap<String, TraceLogGroupStats>,
    values: Vec<TraceLogGroupStats>,
) {
    for value in values {
        let entry = destination
            .entry(value.name.clone())
            .or_insert_with(|| TraceLogGroupStats {
                name: value.name,
                ..TraceLogGroupStats::default()
            });
        entry.rows = entry.rows.saturating_add(value.rows);
        entry.estimated_bytes = entry.estimated_bytes.saturating_add(value.estimated_bytes);
    }
}

fn sorted_groups(
    values: BTreeMap<String, TraceLogGroupStats>,
    limit: usize,
) -> Vec<TraceLogGroupStats> {
    let mut values = values.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        right
            .estimated_bytes
            .cmp(&left.estimated_bytes)
            .then_with(|| right.rows.cmp(&left.rows))
            .then_with(|| left.name.cmp(&right.name))
    });
    values.truncate(limit);
    values
}

fn min_timestamp(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

fn max_timestamp(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn nonnegative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

fn timestamp_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_log_database(path: &Path, rows: &[(i64, &str, &str, i64)]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts INTEGER NOT NULL,
                    level TEXT NOT NULL,
                    target TEXT NOT NULL,
                    feedback_log_body TEXT,
                    estimated_bytes INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
        for (timestamp, level, target, estimated_bytes) in rows {
            connection
                .execute(
                    "INSERT INTO logs(ts, level, target, estimated_bytes)
                     VALUES (?1, ?2, ?3, ?4)",
                    (timestamp, level, target, estimated_bytes),
                )
                .unwrap();
        }
    }

    #[test]
    fn aggregates_current_and_legacy_databases_into_one_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let now = timestamp_seconds() as i64;
        create_log_database(
            &temp.path().join("logs_2.sqlite"),
            &[
                (now - 60, "TRACE", "network", 120),
                (now - 86_400, "INFO", "startup", 80),
            ],
        );
        create_log_database(
            &temp.path().join("sqlite/logs_1.sqlite"),
            &[(now - 120, "TRACE", "network", 300)],
        );

        let snapshot = snapshot(temp.path());

        assert_eq!(snapshot.databases_found, 2);
        assert_eq!(snapshot.databases_scanned, 2);
        assert_eq!(snapshot.row_count, 3);
        assert_eq!(snapshot.estimated_log_bytes, 500);
        assert_eq!(snapshot.recent_row_count, 3);
        assert_eq!(snapshot.recent_estimated_bytes, 500);
        assert!(snapshot.database_bytes > 0);
        assert_eq!(snapshot.levels[0].name, "TRACE");
        assert_eq!(snapshot.levels[0].rows, 2);
        assert_eq!(snapshot.top_targets[0].name, "network");
        assert_eq!(snapshot.top_targets[0].estimated_bytes, 420);
        assert!(snapshot.errors.is_empty());
    }

    #[test]
    fn supports_an_older_log_schema_without_estimated_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("logs_1.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE logs (
                    id INTEGER PRIMARY KEY,
                    ts INTEGER NOT NULL,
                    level TEXT NOT NULL,
                    target TEXT NOT NULL,
                    feedback_log_body TEXT
                );
                 INSERT INTO logs VALUES (1, unixepoch(), 'WARN', 'legacy', '123456');",
            )
            .unwrap();
        drop(connection);

        let snapshot = snapshot(temp.path());

        assert_eq!(snapshot.row_count, 1);
        assert_eq!(snapshot.estimated_log_bytes, 6);
        assert_eq!(snapshot.recent_estimated_bytes, 6);
        assert_eq!(snapshot.levels[0].name, "WARN");
    }

    #[test]
    fn records_schema_errors_without_failing_the_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("logs_2.sqlite"), b"not sqlite").unwrap();

        let snapshot = snapshot(temp.path());

        assert_eq!(snapshot.databases_found, 1);
        assert_eq!(snapshot.databases_scanned, 0);
        assert_eq!(snapshot.errors.len(), 1);
    }

    #[test]
    fn shared_handle_serializes_the_latest_snapshot_without_changing_the_wire_shape() {
        let handle = TraceLogStatsHandle::idle();
        let mut updated = TraceLogStatsSnapshot::default();
        updated.databases_found = 3;
        updated.row_count = 42;
        handle.replace(updated);

        let value = serde_json::to_value(&handle).unwrap();

        assert_eq!(value["pending"], false);
        assert_eq!(value["databasesFound"], 3);
        assert_eq!(value["rowCount"], 42);
        assert!(value.get("snapshot").is_none());
    }

    #[test]
    fn pending_handle_is_distinct_from_a_completed_empty_snapshot() {
        let pending_handle = TraceLogStatsHandle::idle();
        assert!(pending_handle.begin_refresh());
        let pending = serde_json::to_value(pending_handle).unwrap();
        let idle = serde_json::to_value(TraceLogStatsHandle::idle()).unwrap();
        let completed = serde_json::to_value(TraceLogStatsSnapshot::idle()).unwrap();

        assert_eq!(pending["pending"], true);
        assert_eq!(idle["pending"], false);
        assert_eq!(idle["capturedAt"], 0);
        assert_eq!(idle["recentDaysWindow"], RECENT_DAYS);
        assert_eq!(completed["pending"], false);
    }

    #[test]
    fn refresh_can_only_begin_once_until_the_snapshot_is_replaced() {
        let handle = TraceLogStatsHandle::idle();

        assert!(handle.begin_refresh());
        assert!(!handle.begin_refresh());

        handle.replace(TraceLogStatsSnapshot::idle());
        assert!(handle.begin_refresh());
    }
}
