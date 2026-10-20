//! 有界队列排空、批量写入与 NDJSON/SQLite 存储维护。

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use rusqlite::{Connection, params};

use super::{
    FirstByteSource, NDJSON_FILE_NAME, QueuedEntry, RequestInputState, RouteRequestLogStats,
    SQLITE_BUSY_TIMEOUT, SQLITE_FILE_NAME, UpstreamTransport, duration_millis, to_i64,
    unix_timestamp_ms,
};
use crate::config::{RouteRequestLogBackend, RouteRequestLogConfig};
use crate::sqlite_util::table_columns;

const MAX_CONSECUTIVE_WRITE_FAILURES: u32 = 3;

const SQLITE_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub(super) enum WriterControl {
    Shutdown,
}

#[derive(Clone)]
pub(super) struct WorkerConfig {
    backend: RouteRequestLogBackend,
    batch_size: usize,
    flush_interval: Duration,
    max_file_bytes: u64,
    retained_files: usize,
    retention_days: u32,
    root: PathBuf,
}

impl WorkerConfig {
    pub(super) fn new(config: &RouteRequestLogConfig, root: PathBuf) -> Self {
        Self {
            backend: config.backend,
            batch_size: config.batch_size,
            flush_interval: Duration::from_millis(config.flush_interval_ms),
            max_file_bytes: config.max_file_bytes,
            retained_files: config.retained_files,
            retention_days: config.retention_days,
            root,
        }
    }
}

pub(super) fn writer_loop(
    receiver: Receiver<QueuedEntry>,
    control: Receiver<WriterControl>,
    mut sink: BatchSink,
    config: WorkerConfig,
    accepting: &AtomicBool,
    submitting: &AtomicU64,
    stats: &RouteRequestLogStats,
) {
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut deadline = Instant::now() + config.flush_interval;
    let mut consecutive_failures = 0_u32;
    loop {
        if control.try_recv().is_ok() {
            drain_writer_for_shutdown(
                &receiver,
                &mut sink,
                &mut batch,
                submitting,
                stats,
                &mut consecutive_failures,
            );
            return;
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(50));
        match receiver.recv_timeout(wait) {
            Ok(entry) => {
                batch.push(entry);
                while batch.len() < config.batch_size {
                    match receiver.try_recv() {
                        Ok(entry) => batch.push(entry),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => break,
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if !batch.is_empty() {
                    let _ = flush_batch(&mut sink, &mut batch, stats, &mut consecutive_failures);
                }
                let _ = sink.finish();
                return;
            }
        }
        if batch.len() >= config.batch_size || Instant::now() >= deadline {
            if !batch.is_empty()
                && !flush_batch(&mut sink, &mut batch, stats, &mut consecutive_failures)
            {
                accepting.store(false, Ordering::Release);
                stats
                    .write_dropped
                    .fetch_add(receiver.try_iter().count() as u64, Ordering::Relaxed);
                return;
            }
            deadline = Instant::now() + config.flush_interval;
        }
        if let BatchSink::Sqlite(sqlite) = &mut sink {
            sqlite.prune_if_due();
        }
    }
}

fn drain_writer_for_shutdown(
    receiver: &Receiver<QueuedEntry>,
    sink: &mut BatchSink,
    batch: &mut Vec<QueuedEntry>,
    submitting: &AtomicU64,
    stats: &RouteRequestLogStats,
    consecutive_failures: &mut u32,
) {
    loop {
        while batch.len() < batch.capacity() {
            match receiver.try_recv() {
                Ok(entry) => batch.push(entry),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if !batch.is_empty() && !flush_batch(sink, batch, stats, consecutive_failures) {
            break;
        }
        if submitting.load(Ordering::Acquire) == 0 {
            match receiver.try_recv() {
                Ok(entry) => {
                    batch.push(entry);
                    continue;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        thread::yield_now();
    }
    stats
        .write_dropped
        .fetch_add(receiver.try_iter().count() as u64, Ordering::Relaxed);
    let _ = sink.finish();
}

fn flush_batch(
    sink: &mut BatchSink,
    batch: &mut Vec<QueuedEntry>,
    stats: &RouteRequestLogStats,
    consecutive_failures: &mut u32,
) -> bool {
    let now = Instant::now();
    for queued in batch.iter_mut() {
        queued.entry.queue_delay_ms =
            duration_millis(now.saturating_duration_since(queued.enqueued_at));
    }
    // SQLite batches are atomic and request IDs make retries idempotent. NDJSON
    // can have a partially written batch, so it must not be blindly replayed.
    let attempts = if matches!(sink, BatchSink::Sqlite(_)) {
        3
    } else {
        1
    };
    let mut outcome = Ok(());
    for attempt in 0..attempts {
        outcome = sink.write_batch(batch);
        if outcome.is_ok() {
            break;
        }
        stats.write_failures.fetch_add(1, Ordering::Relaxed);
        if attempt + 1 < attempts {
            thread::sleep(Duration::from_millis(25 * (attempt + 1)));
        }
    }
    match outcome {
        Ok(()) => {
            stats
                .entries_written
                .fetch_add(batch.len() as u64, Ordering::Relaxed);
            *consecutive_failures = 0;
        }
        Err(_) => {
            stats
                .write_dropped
                .fetch_add(batch.len() as u64, Ordering::Relaxed);
            *consecutive_failures = consecutive_failures.saturating_add(1);
        }
    }
    batch.clear();
    *consecutive_failures < MAX_CONSECUTIVE_WRITE_FAILURES
}

pub(super) enum BatchSink {
    Ndjson(NdjsonSink),
    Sqlite(SqliteSink),
}

impl BatchSink {
    pub(super) fn open(config: &WorkerConfig) -> std::io::Result<Self> {
        fs::create_dir_all(&config.root)?;
        match config.backend {
            RouteRequestLogBackend::Ndjson => NdjsonSink::open(
                config.root.join(NDJSON_FILE_NAME),
                config.max_file_bytes,
                config.retained_files,
            )
            .map(Self::Ndjson),
            RouteRequestLogBackend::Sqlite => {
                SqliteSink::open(&config.root.join(SQLITE_FILE_NAME), config.retention_days)
                    .map(Self::Sqlite)
                    .map_err(std::io::Error::other)
            }
        }
    }

    fn write_batch(&mut self, batch: &[QueuedEntry]) -> anyhow::Result<()> {
        match self {
            Self::Ndjson(sink) => sink.write_batch(batch),
            Self::Sqlite(sink) => sink.write_batch(batch),
        }
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Ndjson(sink) => sink.finish(),
            Self::Sqlite(sink) => sink.finish(),
        }
    }
}

pub(super) struct NdjsonSink {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    written_bytes: u64,
    max_file_bytes: u64,
    retained_files: usize,
}

impl NdjsonSink {
    fn open(path: PathBuf, max_file_bytes: u64, retained_files: usize) -> std::io::Result<Self> {
        let file = open_private_append_file(&path)?;
        let written_bytes = file.metadata()?.len();
        Ok(Self {
            path,
            writer: Some(BufWriter::new(file)),
            written_bytes,
            max_file_bytes,
            retained_files,
        })
    }

    fn write_batch(&mut self, batch: &[QueuedEntry]) -> anyhow::Result<()> {
        let mut encoded = Vec::with_capacity(batch.len().saturating_mul(512));
        for queued in batch {
            serde_json::to_writer(&mut encoded, &queued.entry)?;
            encoded.push(b'\n');
        }
        if self.written_bytes > 0
            && self.written_bytes.saturating_add(encoded.len() as u64) > self.max_file_bytes
        {
            self.rotate()?;
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| std::io::Error::other("NDJSON writer is unavailable"))?;
        writer.write_all(&encoded)?;
        writer.flush()?;
        self.written_bytes = self.written_bytes.saturating_add(encoded.len() as u64);
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        // Windows does not allow an open file to be renamed. Take and drop the
        // writer before rotating so the same implementation is portable.
        if let Some(mut writer) = self.writer.take() {
            writer.flush()?;
            drop(writer);
        }
        if self.retained_files > 0 {
            let oldest = rotated_path(&self.path, self.retained_files);
            crate::fs_util::remove_file_if_exists(&oldest)?;
            for index in (1..self.retained_files).rev() {
                let source = rotated_path(&self.path, index);
                let destination = rotated_path(&self.path, index + 1);
                rename_if_exists(&source, &destination)?;
            }
            rename_if_exists(&self.path, &rotated_path(&self.path, 1))?;
        } else {
            crate::fs_util::remove_file_if_exists(&self.path)?;
        }
        let file = open_private_append_file(&self.path)?;
        self.writer = Some(BufWriter::new(file));
        self.written_bytes = 0;
        Ok(())
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }
}

pub(super) struct SqliteSink {
    connection: Connection,
    retention_ms: u64,
    next_prune_at: Instant,
}

impl SqliteSink {
    pub(super) fn open(path: &Path, retention_days: u32) -> anyhow::Result<Self> {
        ensure_private_sqlite_file(path)?;
        let connection = Connection::open(path)?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS route_request_logs (
                request_id TEXT PRIMARY KEY,
                trace_id TEXT NOT NULL,
                timestamp_unix_ms INTEGER NOT NULL,
                provider TEXT,
                provider_name TEXT,
                official_account_id TEXT,
                requested_model TEXT NOT NULL,
                model TEXT,
                reasoning_effort TEXT,
                thinking_budget_tokens INTEGER,
                ttft_ms INTEGER,
                router_pre_upstream_ms INTEGER,
                upstream_first_byte_ms INTEGER,
                downstream_first_content_ms INTEGER,
                upstream_header_ms INTEGER,
                total_duration_ms INTEGER NOT NULL,
                queue_delay_ms INTEGER NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cached_input_tokens INTEGER,
                cache_creation_input_tokens INTEGER,
                reasoning_output_tokens INTEGER,
                total_tokens INTEGER,
                usage_reported INTEGER NOT NULL,
                usage_unavailable_reason TEXT,
                request_protocol TEXT NOT NULL,
                upstream_transport TEXT,
                request_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                status_code INTEGER,
                upstream_status_code INTEGER,
                error_code TEXT,
                upstream_error_summary TEXT,
                completion_reason TEXT,
                fallback_count INTEGER NOT NULL,
                fallback_reason TEXT,
                upstream_authority TEXT,
                upstream_request_id TEXT,
                upstream_protocol TEXT,
                protocol_bridge TEXT,
                first_byte_source TEXT,
                client_fingerprint TEXT,
                subagent INTEGER NOT NULL,
                schema_version INTEGER NOT NULL,
                codex_session_id TEXT,
                codex_session_is_parent INTEGER NOT NULL,
                requested_service_tier TEXT,
                service_tier TEXT,
                upstream_request_headers TEXT,
                upstream_response_headers TEXT,
                request_input_state TEXT,
                request_input_items INTEGER,
                request_has_previous_response_id INTEGER,
                request_bytes INTEGER,
                upstream_input_state TEXT,
                upstream_input_items INTEGER,
                upstream_has_previous_response_id INTEGER,
                upstream_bytes INTEGER,
                upstream_response_model TEXT,
                concurrency_wait_ms INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_time_id
                ON route_request_logs(timestamp_unix_ms DESC, request_id DESC);
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_provider_time
                ON route_request_logs(provider COLLATE NOCASE, timestamp_unix_ms DESC, request_id DESC);
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_model_time
                ON route_request_logs(COALESCE(model, requested_model) COLLATE NOCASE,
                    timestamp_unix_ms DESC, request_id DESC);
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_session_time
                ON route_request_logs(codex_session_id, timestamp_unix_ms DESC, request_id DESC);
             DROP INDEX IF EXISTS idx_route_request_logs_time;
             DROP INDEX IF EXISTS idx_route_request_logs_provider_model;
             DROP INDEX IF EXISTS idx_route_request_logs_status;
             CREATE INDEX IF NOT EXISTS idx_route_request_logs_status_time_id
                ON route_request_logs(status, timestamp_unix_ms DESC, request_id DESC);",
        )?;
        let existing_columns = table_columns(&connection, "route_request_logs")?;
        for (column, column_type) in [
            ("requested_service_tier", "TEXT"),
            ("service_tier", "TEXT"),
            ("upstream_request_headers", "TEXT"),
            ("upstream_response_headers", "TEXT"),
            ("official_account_id", "TEXT"),
            ("request_input_state", "TEXT"),
            ("request_input_items", "INTEGER"),
            ("request_has_previous_response_id", "INTEGER"),
            ("request_bytes", "INTEGER"),
            ("upstream_input_state", "TEXT"),
            ("upstream_input_items", "INTEGER"),
            ("upstream_has_previous_response_id", "INTEGER"),
            ("upstream_bytes", "INTEGER"),
            ("upstream_response_model", "TEXT"),
            ("concurrency_wait_ms", "INTEGER"),
            ("route_decision", "TEXT"),
            ("upstream_attempts", "TEXT"),
            ("attempts_omitted", "INTEGER"),
        ] {
            if !existing_columns.contains(column) {
                connection.execute_batch(&format!(
                    "ALTER TABLE route_request_logs ADD COLUMN {column} {column_type}"
                ))?;
            }
        }
        let retention_ms = u64::from(retention_days)
            .saturating_mul(24 * 60 * 60)
            .saturating_mul(1_000);
        let removed = prune_sqlite_logs(&connection, retention_ms)?;
        Ok(Self {
            connection,
            retention_ms,
            next_prune_at: Instant::now()
                + if removed == 1_000 {
                    Duration::from_secs(1)
                } else {
                    SQLITE_PRUNE_INTERVAL
                },
        })
    }

    pub(super) fn write_batch(&mut self, batch: &[QueuedEntry]) -> anyhow::Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO route_request_logs (
                    request_id, trace_id, timestamp_unix_ms, provider, provider_name,
                    requested_model, model, reasoning_effort, thinking_budget_tokens,
                    ttft_ms, router_pre_upstream_ms, upstream_first_byte_ms,
                    downstream_first_content_ms, upstream_header_ms, total_duration_ms, queue_delay_ms,
                    input_tokens, output_tokens, cached_input_tokens,
                    cache_creation_input_tokens, reasoning_output_tokens, total_tokens,
                    usage_reported, usage_unavailable_reason, request_protocol,
                    upstream_transport, request_kind,
                    status, status_code, upstream_status_code, error_code, completion_reason,
                    fallback_count, fallback_reason, upstream_authority,
                    upstream_request_id, upstream_protocol, protocol_bridge,
                    first_byte_source, client_fingerprint, subagent, schema_version,
                    upstream_error_summary, codex_session_id, codex_session_is_parent,
                    requested_service_tier, service_tier, upstream_request_headers,
                    upstream_response_headers,
                    official_account_id, request_input_state, request_input_items,
                    request_has_previous_response_id, request_bytes,
                    upstream_input_state, upstream_input_items,
                    upstream_has_previous_response_id, upstream_bytes,
                    upstream_response_model, concurrency_wait_ms,
                    route_decision, upstream_attempts, attempts_omitted
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                    ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                    ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                    ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40,
                    ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, ?50,
                    ?51, ?52, ?53, ?54, ?55, ?56, ?57, ?58, ?59, ?60, ?61, ?62, ?63
                ) ON CONFLICT(request_id) DO NOTHING",
            )?;
            for queued in batch {
                let entry = &queued.entry;
                statement.execute(params![
                    entry.request_id,
                    entry.trace_id,
                    to_i64(entry.timestamp_unix_ms),
                    entry.provider,
                    entry.provider_name,
                    entry.requested_model,
                    entry.model,
                    entry.reasoning_effort,
                    entry.thinking_budget_tokens.map(to_i64),
                    entry.ttft_ms.map(to_i64),
                    entry.router_pre_upstream_ms.map(to_i64),
                    entry.upstream_first_byte_ms.map(to_i64),
                    entry.downstream_first_content_ms.map(to_i64),
                    entry.upstream_header_ms.map(to_i64),
                    to_i64(entry.total_duration_ms),
                    to_i64(entry.queue_delay_ms),
                    entry.token_usage.input_tokens.map(to_i64),
                    entry.token_usage.output_tokens.map(to_i64),
                    entry.token_usage.cached_input_tokens.map(to_i64),
                    entry.token_usage.cache_creation_input_tokens.map(to_i64),
                    entry.token_usage.reasoning_output_tokens.map(to_i64),
                    entry.token_usage.total_tokens.map(to_i64),
                    entry.usage_reported,
                    entry.usage_unavailable_reason,
                    entry.request_protocol.as_str(),
                    entry.upstream_transport.map(UpstreamTransport::as_str),
                    entry.request_kind,
                    entry.status.as_str(),
                    entry.status_code,
                    entry.upstream_status_code,
                    entry.error_code,
                    entry.completion_reason,
                    entry.fallback_count,
                    entry.fallback_reason,
                    entry.upstream_authority,
                    entry.upstream_request_id,
                    entry.upstream_protocol,
                    entry.protocol_bridge,
                    entry.first_byte_source.map(FirstByteSource::as_str),
                    entry.client_fingerprint,
                    entry.subagent,
                    entry.schema_version,
                    entry.upstream_error_summary,
                    entry.codex_session_id,
                    entry.codex_session_is_parent,
                    entry.requested_service_tier,
                    entry.service_tier,
                    entry.upstream_request_headers,
                    entry.upstream_response_headers,
                    entry.official_account_id,
                    entry.request_input_state.map(RequestInputState::as_str),
                    entry.request_input_items.map(to_i64),
                    entry.request_has_previous_response_id,
                    entry.request_bytes.map(to_i64),
                    entry.upstream_input_state.map(RequestInputState::as_str),
                    entry.upstream_input_items.map(to_i64),
                    entry.upstream_has_previous_response_id,
                    entry.upstream_bytes.map(to_i64),
                    entry.upstream_response_model,
                    entry.concurrency_wait_ms.map(to_i64),
                    entry
                        .route_decision
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    serde_json::to_string(&entry.upstream_attempts)?,
                    entry.attempts_omitted,
                ])?;
            }
        }
        transaction.commit()?;
        self.prune_if_due();
        Ok(())
    }

    fn prune_if_due(&mut self) {
        if Instant::now() >= self.next_prune_at {
            // Retention is maintenance, not part of accepting the current
            // batch. A prune failure must not misreport committed rows as lost.
            let removed = prune_sqlite_logs(&self.connection, self.retention_ms);
            self.next_prune_at = Instant::now()
                + match removed {
                    Ok(1_000) => Duration::from_secs(1),
                    Err(_) => Duration::from_secs(60),
                    _ => SQLITE_PRUNE_INTERVAL,
                };
        }
    }

    pub(super) fn finish(&mut self) -> anyhow::Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        Ok(())
    }
}

fn open_private_append_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn ensure_private_sqlite_file(path: &Path) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let _file = options.open(path)?;
    #[cfg(unix)]
    {
        let mut permissions = _file.metadata()?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn prune_sqlite_logs(connection: &Connection, retention_ms: u64) -> rusqlite::Result<usize> {
    let cutoff = unix_timestamp_ms().saturating_sub(retention_ms);
    connection.execute(
        "DELETE FROM route_request_logs WHERE rowid IN (
            SELECT rowid FROM route_request_logs WHERE timestamp_unix_ms < ?1
            ORDER BY timestamp_unix_ms LIMIT 1000)",
        [to_i64(cutoff)],
    )
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(NDJSON_FILE_NAME));
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

fn rename_if_exists(source: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
