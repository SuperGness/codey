//! 持久化通知元数据和各渠道的投递状态；认领事务提交后才允许发送网络请求。

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use super::NotificationEvent;

const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const DELIVERY_LIFETIME_MS: i64 = 24 * 60 * 60 * 1_000;
const MAX_ATTEMPTS: u32 = 12;
const MAX_BACKOFF_MS: u64 = 5 * 60 * 1_000;
const MAX_EVENT_BYTES: usize = 16 * 1_024;
const MAX_CLAIM_BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryTarget {
    pub(crate) channel_id: String,
    /// 渠道绑定的摘要，不包含地址、接收人或凭据原文。
    pub(crate) binding_hash: String,
}

#[derive(Debug, Clone)]
pub(crate) struct OutboxDelivery {
    pub(crate) delivery_key: String,
    pub(crate) event_key: String,
    pub(crate) event: NotificationEvent,
    pub(crate) target: DeliveryTarget,
    pub(crate) attempt: u32,
    pub(crate) created_at_ms: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DeliveryOutcome {
    Delivered,
    Retryable { retry_after_ms: Option<u64> },
    Uncertain,
    PermanentFailure,
    Cancelled,
}

#[derive(Clone, Copy)]
struct Limits {
    pending: usize,
    events: usize,
    deliveries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            pending: 10_000,
            events: 50_000,
            deliveries: 100_000,
        }
    }
}

type OpenSlot = Mutex<Weak<Inner>>;

struct Inner {
    connection: Mutex<Connection>,
    limits: Limits,
    // 阻止其他进程重置本进程正在发送的记录。
    _process_lock: File,
    // 队列仍在使用时，保留同一路径的初始化锁和共享连接入口。
    _open_slot: Arc<OpenSlot>,
}

#[derive(Clone)]
pub(crate) struct Outbox {
    inner: Arc<Inner>,
}

impl Outbox {
    pub(crate) async fn open(path: PathBuf) -> Result<Self, String> {
        tokio::task::spawn_blocking(move || open_blocking(path, Limits::default()))
            .await
            .map_err(|error| format!("通知队列初始化任务失败：{error}"))?
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection, Limits) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut connection = inner
                .connection
                .lock()
                .map_err(|_| "通知队列连接锁损坏".to_string())?;
            operation(&mut connection, inner.limits)
        })
        .await
        .map_err(|error| format!("通知队列任务失败：{error}"))?
    }

    /// 已有事件保留首次入队的渠道集合，包括空集合。
    pub(crate) async fn enqueue(
        &self,
        event_key: String,
        mut event: NotificationEvent,
        targets: Vec<DeliveryTarget>,
        now_ms: i64,
    ) -> Result<bool, String> {
        // 当前发送器仅展示事件分类和元数据，不需要保存错误正文。
        event.error = None;
        self.run(move |connection, limits| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db_error)?;
            if event_exists(&transaction, &event_key)? {
                return Ok(false);
            }
            validate_key(&event_key, 4_096, "事件标识")?;
            let mut seen = HashMap::new();
            for target in &targets {
                validate_key(&target.channel_id, 512, "渠道标识")?;
                if target.binding_hash.len() != 64
                    || !target.binding_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err("通知渠道绑定必须使用 SHA-256 摘要".to_string());
                }
                if let Some(previous) = seen.insert(&target.channel_id, &target.binding_hash)
                    && previous != &target.binding_hash
                {
                    return Err("同一通知渠道存在多个绑定".to_string());
                }
            }
            let payload = serde_json::to_string(&event)
                .map_err(|_| "通知事件无法序列化".to_string())?;
            if payload.len() > MAX_EVENT_BYTES {
                return Err("通知事件元数据超过容量限制".to_string());
            }
            // 只清理超过去重保留期且全部结束的事件，不为腾出容量删除待发记录。
            let removed = transaction
                .execute(
                    "DELETE FROM notification_events WHERE settled_at_ms <= ?1",
                    [now_ms.saturating_sub(RETENTION_MS)],
                )
                .map_err(db_error)?;
            if removed > 0 {
                transaction.execute(
                    "DELETE FROM notification_channels WHERE NOT EXISTS (
                        SELECT 1 FROM notification_deliveries d WHERE d.channel_id = notification_channels.channel_id)",
                    [],
                ).map_err(db_error)?;
            }
            let (pending, retained, deliveries): (i64, i64, i64) = transaction
                .query_row(
                    "SELECT
                        (SELECT COUNT(*) FROM notification_deliveries WHERE state IN ('pending', 'inflight')),
                        (SELECT COUNT(*) FROM notification_events),
                        (SELECT COUNT(*) FROM notification_deliveries)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(db_error)?;
            let target_count = seen.len();
            if pending as usize + target_count > limits.pending
                || retained as usize >= limits.events
                || deliveries as usize + target_count > limits.deliveries
            {
                return Err("通知队列已满，事件尚未入队".to_string());
            }
            transaction
                .execute(
                    "INSERT INTO notification_events(event_key, payload, created_at_ms, settled_at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![event_key, payload, now_ms, if targets.is_empty() { Some(now_ms) } else { None }],
                )
                .map_err(db_error)?;
            for (channel_id, binding_hash) in seen {
                let delivery_key = crate::fs_util::sha256_hex_str(
                    &serde_json::to_string(&(&event_key, channel_id)).expect("string tuple serializes"),
                );
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO notification_channels(channel_id, last_claim_seq) VALUES (?1, 0)",
                        [channel_id],
                    )
                    .map_err(db_error)?;
                transaction
                    .execute(
                        "INSERT INTO notification_deliveries
                         (delivery_key, event_key, channel_id, binding_hash, state, attempt,
                          next_attempt_ms, created_at_ms, expires_at_ms)
                         VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?5, ?6)",
                        params![delivery_key, event_key, channel_id, binding_hash, now_ms,
                            now_ms.saturating_add(DELIVERY_LIFETIME_MS)],
                    )
                    .map_err(db_error)?;
            }
            transaction.commit().map_err(db_error)?;
            Ok(true)
        })
        .await
    }

    pub(crate) async fn has_event(&self, event_key: String) -> Result<bool, String> {
        self.run(move |connection, _| event_exists(connection, &event_key))
            .await
    }

    pub(crate) async fn record_baseline_keys(
        &self,
        keys: Vec<String>,
        now_ms: i64,
    ) -> Result<(), String> {
        if keys.is_empty() {
            return Ok(());
        }
        self.run(move |connection, _| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db_error)?;
            let mut statement = transaction
                .prepare(
                    "INSERT OR IGNORE INTO notification_events(event_key, payload, created_at_ms, settled_at_ms)
                     VALUES (?1, '{}', ?2, ?2)",
                )
                .map_err(db_error)?;
            for key in keys {
                validate_key(&key, 4_096, "事件标识")?;
                statement.execute(params![key, now_ms]).map_err(db_error)?;
            }
            drop(statement);
            transaction.commit().map_err(db_error)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn claim_due(
        &self,
        now_ms: i64,
        limit: usize,
        busy_channels: Vec<String>,
    ) -> Result<Vec<OutboxDelivery>, String> {
        self.run(move |connection, _| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db_error)?;
            let expired = {
                let mut statement = transaction
                    .prepare(
                        "UPDATE notification_deliveries SET state = 'permanent_failed', settled_at_ms = ?1,
                         settled_reason = 'expired' WHERE state = 'pending' AND expires_at_ms <= ?1
                         RETURNING event_key",
                    )
                    .map_err(db_error)?;
                statement
                    .query_map([now_ms], |row| row.get::<_, String>(0))
                    .map_err(db_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(db_error)?
            };
            for event_key in expired {
                settle_event(&transaction, &event_key, now_ms)?;
            }
            let busy: HashSet<_> = busy_channels.into_iter().collect();
            let candidates = {
                let mut statement = transaction.prepare(
                    "WITH eligible AS (
                        SELECT d.*, ROW_NUMBER() OVER (
                            PARTITION BY d.channel_id ORDER BY d.next_attempt_ms, d.created_at_ms, d.delivery_key
                        ) AS channel_rank
                        FROM notification_deliveries d
                        WHERE d.state = 'pending' AND d.next_attempt_ms <= ?1
                        AND NOT EXISTS (SELECT 1 FROM notification_deliveries active
                            WHERE active.channel_id = d.channel_id AND active.state = 'inflight')
                    )
                    SELECT d.delivery_key, d.event_key, e.payload, d.channel_id, d.binding_hash,
                        d.attempt, d.created_at_ms
                    FROM eligible d JOIN notification_events e USING (event_key)
                    JOIN notification_channels c USING (channel_id)
                    WHERE d.channel_rank = 1
                    ORDER BY c.last_claim_seq, d.next_attempt_ms, d.created_at_ms, d.delivery_key",
                ).map_err(db_error)?;
                let rows = statement.query_map([now_ms], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, u32>(5)?, row.get::<_, i64>(6)?))
                }).map_err(db_error)?;
                let mut candidates = Vec::new();
                for row in rows {
                    if candidates.len() >= limit.min(MAX_CLAIM_BATCH) {
                        break;
                    }
                    let (delivery_key, event_key, payload, channel_id, binding_hash, attempt, created_at_ms) = row.map_err(db_error)?;
                    if busy.contains(&channel_id) {
                        continue;
                    }
                    let event = serde_json::from_str(&payload)
                        .map_err(|_| "通知队列事件数据损坏，已停止发送".to_string())?;
                    candidates.push(OutboxDelivery {
                        delivery_key, event_key, event,
                        target: DeliveryTarget { channel_id, binding_hash },
                        attempt: attempt.saturating_add(1), created_at_ms,
                    });
                }
                candidates
            };
            let mut sequence: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(last_claim_seq), 0) FROM notification_channels", [], |row| row.get(0),
            ).map_err(db_error)?;
            for delivery in &candidates {
                transaction.execute(
                    "UPDATE notification_deliveries SET state = 'inflight', attempt = ?1
                     WHERE delivery_key = ?2 AND state = 'pending'", params![delivery.attempt, delivery.delivery_key],
                ).map_err(db_error)?;
                sequence = sequence.saturating_add(1);
                transaction.execute(
                    "UPDATE notification_channels SET last_claim_seq = ?1 WHERE channel_id = ?2",
                    params![sequence, delivery.target.channel_id],
                ).map_err(db_error)?;
            }
            transaction.commit().map_err(db_error)?;
            Ok(candidates)
        }).await
    }

    /// 旧尝试不能覆盖新认领；提交失败也不能视为允许重发。
    pub(crate) async fn finish(
        &self,
        delivery: &OutboxDelivery,
        outcome: DeliveryOutcome,
        now_ms: i64,
    ) -> Result<(), String> {
        let delivery = delivery.clone();
        self.run(move |connection, _| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(db_error)?;
            let (state, reason, next_attempt_ms) = match outcome {
                DeliveryOutcome::Delivered => ("delivered", "delivered", now_ms),
                DeliveryOutcome::Uncertain => ("uncertain", "outcome_uncertain", now_ms),
                DeliveryOutcome::PermanentFailure => ("permanent_failed", "permanent_failure", now_ms),
                DeliveryOutcome::Cancelled => ("cancelled", "cancelled", now_ms),
                DeliveryOutcome::Retryable { .. } if delivery.attempt >= MAX_ATTEMPTS =>
                    ("permanent_failed", "attempt_limit", now_ms),
                DeliveryOutcome::Retryable { .. } if now_ms >= delivery.created_at_ms.saturating_add(DELIVERY_LIFETIME_MS) =>
                    ("permanent_failed", "expired", now_ms),
                DeliveryOutcome::Retryable { retry_after_ms } => {
                    let delay = retry_delay_ms(&delivery.delivery_key, delivery.attempt, retry_after_ms);
                    let due = now_ms.saturating_add(delay.min(i64::MAX as u64) as i64)
                        .min(delivery.created_at_ms.saturating_add(DELIVERY_LIFETIME_MS));
                    ("pending", "retry_scheduled", due)
                }
            };
            let changed = transaction.execute(
                "UPDATE notification_deliveries SET state = ?1, settled_reason = ?2, next_attempt_ms = ?3, settled_at_ms = ?4
                 WHERE delivery_key = ?5 AND event_key = ?6 AND channel_id = ?7 AND binding_hash = ?8
                 AND state = 'inflight' AND attempt = ?9",
                params![state, reason, next_attempt_ms, if state == "pending" { None } else { Some(now_ms) },
                    delivery.delivery_key, delivery.event_key, delivery.target.channel_id, delivery.target.binding_hash, delivery.attempt],
            ).map_err(db_error)?;
            if changed != 1 {
                return Err("通知投递认领已过期，未修改队列状态".to_string());
            }
            settle_event(&transaction, &delivery.event_key, now_ms)?;
            transaction.commit().map_err(db_error)
        }).await
    }

    pub(crate) async fn next_due_ms(
        &self,
        excluded_channels: Vec<String>,
    ) -> Result<Option<i64>, String> {
        self.run(move |connection, _| {
            let excluded: HashSet<_> = excluded_channels.into_iter().collect();
            let mut statement = connection
                .prepare(
                    "SELECT d.channel_id, MIN(d.next_attempt_ms) FROM notification_deliveries d
                 WHERE d.state = 'pending' AND NOT EXISTS (
                    SELECT 1 FROM notification_deliveries active
                    WHERE active.channel_id = d.channel_id AND active.state = 'inflight')
                 GROUP BY d.channel_id",
                )
                .map_err(db_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(db_error)?;
            let mut next = None;
            for row in rows {
                let (channel, due) = row.map_err(db_error)?;
                if !excluded.contains(&channel) {
                    next = Some(next.map_or(due, |current: i64| current.min(due)));
                }
            }
            Ok(next)
        })
        .await
    }

    pub(crate) async fn mark_inflight_uncertain(&self, now_ms: i64) -> Result<(), String> {
        self.run(move |connection, _| recover_inflight(connection, now_ms))
            .await
    }
}

fn db_error(error: rusqlite::Error) -> String {
    format!("通知队列存储失败：{error}")
}

fn validate_key(key: &str, max_bytes: usize, label: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > max_bytes || key.contains('\0') {
        Err(format!("通知{label}无效"))
    } else {
        Ok(())
    }
}

fn event_exists(connection: &Connection, event_key: &str) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT 1 FROM notification_events WHERE event_key = ?1",
            [event_key],
            |_| Ok(()),
        )
        .optional()
        .map(|result| result.is_some())
        .map_err(db_error)
}

fn settle_event(connection: &Connection, event_key: &str, now_ms: i64) -> Result<(), String> {
    connection.execute(
        "UPDATE notification_events SET settled_at_ms = ?1 WHERE event_key = ?2 AND settled_at_ms IS NULL
         AND NOT EXISTS (SELECT 1 FROM notification_deliveries WHERE event_key = ?2 AND state IN ('pending', 'inflight'))",
        params![now_ms, event_key],
    ).map_err(db_error)?;
    Ok(())
}

fn recover_inflight(connection: &mut Connection, now_ms: i64) -> Result<(), String> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    let affected = {
        let mut statement = transaction.prepare(
            "UPDATE notification_deliveries SET state = 'uncertain', settled_at_ms = ?1, settled_reason = 'interrupted'
             WHERE state = 'inflight' RETURNING event_key",
        ).map_err(db_error)?;
        statement
            .query_map([now_ms], |row| row.get::<_, String>(0))
            .map_err(db_error)?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(db_error)?
    };
    for event_key in affected {
        settle_event(&transaction, &event_key, now_ms)?;
    }
    transaction.commit().map_err(db_error)
}

fn retry_delay_ms(key: &str, attempt: u32, retry_after_ms: Option<u64>) -> u64 {
    let base = 1_000_u64
        .saturating_mul(1_u64 << attempt.saturating_sub(1).min(20))
        .min(MAX_BACKOFF_MS);
    let digest = crate::fs_util::sha256_hex_str(&format!("{key}:{attempt}"));
    let jitter = u64::from_str_radix(&digest[..16], 16).unwrap_or_default() % (base / 4 + 1);
    base.saturating_add(jitter)
        .min(MAX_BACKOFF_MS)
        .max(retry_after_ms.unwrap_or_default())
}

fn private_file(path: &Path) -> Result<File, String> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err("通知队列路径必须是普通文件".to_string());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("无法打开通知队列文件：{error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("无法设置通知队列文件权限：{error}"))?;
    }
    Ok(file)
}

fn open_slot(path: &Path) -> Result<Arc<OpenSlot>, String> {
    static OPEN: OnceLock<Mutex<HashMap<PathBuf, Weak<OpenSlot>>>> = OnceLock::new();
    let mut registry = OPEN
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "通知队列初始化锁损坏".to_string())?;
    registry.retain(|_, value| value.strong_count() > 0);
    if let Some(slot) = registry.get(path).and_then(Weak::upgrade) {
        return Ok(slot);
    }
    let slot = Arc::new(Mutex::new(Weak::new()));
    registry.insert(path.to_path_buf(), Arc::downgrade(&slot));
    Ok(slot)
}

fn open_blocking(path: PathBuf, limits: Limits) -> Result<Outbox, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .map_err(|error| format!("无法创建通知队列目录：{error}"))?;
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("无法解析通知队列目录：{error}"))?;
    let filename = path
        .file_name()
        .ok_or_else(|| "通知队列文件名无效".to_string())?;
    let path = parent.join(filename);
    // 仅同一路径串行初始化，避免独立队列等待其他数据库的磁盘同步。
    let slot = open_slot(&path)?;
    let mut registered = slot
        .lock()
        .map_err(|_| "通知队列初始化锁损坏".to_string())?;
    if let Some(inner) = registered.upgrade() {
        return Ok(Outbox { inner });
    }
    let mut lock_name = filename.to_os_string();
    lock_name.push(".lock");
    let process_lock = private_file(&parent.join(lock_name))?;
    fs2::FileExt::try_lock_exclusive(&process_lock).map_err(|error| {
        if crate::fs_util::file_lock_is_contended(&error) {
            "通知队列正在被另一个进程使用".to_string()
        } else {
            format!("无法锁定通知队列：{error}")
        }
    })?;
    private_file(&path)?;
    let mut connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(db_error)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(db_error)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = FULL;
         BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS notification_events (
            event_key TEXT PRIMARY KEY, payload TEXT NOT NULL, created_at_ms INTEGER NOT NULL, settled_at_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS notification_events_settled ON notification_events(settled_at_ms)
            WHERE settled_at_ms IS NOT NULL;
         CREATE TABLE IF NOT EXISTS notification_channels (channel_id TEXT PRIMARY KEY, last_claim_seq INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS notification_deliveries (
            delivery_key TEXT PRIMARY KEY,
            event_key TEXT NOT NULL REFERENCES notification_events(event_key) ON DELETE CASCADE,
            channel_id TEXT NOT NULL, binding_hash TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('pending', 'inflight', 'delivered', 'uncertain', 'permanent_failed', 'cancelled')),
            attempt INTEGER NOT NULL, next_attempt_ms INTEGER NOT NULL, created_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER NOT NULL, settled_at_ms INTEGER, settled_reason TEXT,
            UNIQUE(event_key, channel_id)
         );
         CREATE INDEX IF NOT EXISTS notification_deliveries_due ON notification_deliveries(next_attempt_ms, channel_id)
            WHERE state = 'pending';
         CREATE INDEX IF NOT EXISTS notification_deliveries_active ON notification_deliveries(state)
            WHERE state IN ('pending', 'inflight');
         CREATE INDEX IF NOT EXISTS notification_deliveries_channel ON notification_deliveries(channel_id, state);
         CREATE INDEX IF NOT EXISTS notification_deliveries_event ON notification_deliveries(event_key, state);
         COMMIT;"
    ).map_err(db_error)?;
    recover_inflight(
        &mut connection,
        crate::fs_util::timestamp_millis().min(i64::MAX as u128) as i64,
    )?;
    let inner = Arc::new(Inner {
        connection: Mutex::new(connection),
        limits,
        _process_lock: process_lock,
        _open_slot: Arc::clone(&slot),
    });
    *registered = Arc::downgrade(&inner);
    Ok(Outbox { inner })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> NotificationEvent {
        NotificationEvent::new(
            "session.failed",
            "session",
            "profile",
            "Codex",
            10,
            Some("private-raw-error-body".to_string()),
        )
    }

    fn target(channel: &str) -> DeliveryTarget {
        DeliveryTarget {
            channel_id: channel.to_string(),
            binding_hash: crate::fs_util::sha256_hex_str(channel),
        }
    }

    async fn queue() -> (tempfile::TempDir, Outbox) {
        let directory = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(directory.path().join("notifications.db"))
            .await
            .unwrap();
        (directory, outbox)
    }

    async fn state(outbox: &Outbox, channel: &str) -> (String, u32, Option<String>) {
        let channel = channel.to_string();
        outbox.run(move |connection, _| {
            connection.query_row("SELECT state, attempt, settled_reason FROM notification_deliveries WHERE channel_id = ?1", [channel],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(db_error)
        }).await.unwrap()
    }

    #[tokio::test]
    async fn enqueue_is_unique_and_freezes_original_channels() {
        let (_directory, outbox) = queue().await;
        assert!(
            outbox
                .enqueue("event".into(), event(), vec![target("a"), target("a")], 100)
                .await
                .unwrap()
        );
        assert!(
            !outbox
                .enqueue("event".into(), event(), vec![target("b")], 200)
                .await
                .unwrap()
        );
        let deliveries = outbox.claim_due(200, 10, vec![]).await.unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].target.channel_id, "a");
        assert_eq!(deliveries[0].attempt, 1);
        assert!(deliveries[0].event.error.is_none());
        assert!(outbox.has_event("event".into()).await.unwrap());
    }

    #[tokio::test]
    async fn successful_channel_is_not_resent_after_reopening() {
        let (directory, outbox) = queue().await;
        outbox
            .enqueue("event".into(), event(), vec![target("a"), target("b")], 100)
            .await
            .unwrap();
        let first = outbox
            .claim_due(100, 1, vec!["b".into()])
            .await
            .unwrap()
            .remove(0);
        outbox
            .finish(&first, DeliveryOutcome::Delivered, 101)
            .await
            .unwrap();
        drop(outbox);
        let reopened = Outbox::open(directory.path().join("notifications.db"))
            .await
            .unwrap();
        let deliveries = reopened.claim_due(102, 10, vec![]).await.unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].target.channel_id, "b");
        assert_eq!(state(&reopened, "a").await.0, "delivered");
    }

    #[tokio::test]
    async fn interrupted_delivery_becomes_uncertain_and_never_retries() {
        let (directory, outbox) = queue().await;
        outbox
            .enqueue("event".into(), event(), vec![target("a")], 100)
            .await
            .unwrap();
        assert_eq!(outbox.claim_due(100, 1, vec![]).await.unwrap().len(), 1);
        drop(outbox);
        let reopened = Outbox::open(directory.path().join("notifications.db"))
            .await
            .unwrap();
        assert_eq!(state(&reopened, "a").await.0, "uncertain");
        assert!(
            reopened
                .claim_due(i64::MAX, 1, vec![])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(reopened.next_due_ms(vec![]).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clones_and_repeated_open_do_not_recover_active_claims() {
        let (directory, outbox) = queue().await;
        outbox
            .enqueue("event".into(), event(), vec![target("a")], 100)
            .await
            .unwrap();
        let delivery = outbox.claim_due(100, 1, vec![]).await.unwrap().remove(0);
        let cloned = outbox.clone();
        let reopened = Outbox::open(directory.path().join("notifications.db"))
            .await
            .unwrap();
        assert_eq!(state(&reopened, "a").await.0, "inflight");
        cloned
            .finish(&delivery, DeliveryOutcome::Delivered, 101)
            .await
            .unwrap();
        assert_eq!(state(&reopened, "a").await.0, "delivered");
    }

    #[tokio::test]
    async fn concurrent_opens_share_the_same_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notifications.db");
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut opens = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            opens.spawn(async move {
                barrier.wait().await;
                Outbox::open(path).await.unwrap()
            });
        }
        let first = opens.join_next().await.unwrap().unwrap();
        while let Some(opened) = opens.join_next().await {
            assert!(Arc::ptr_eq(&first.inner, &opened.unwrap().inner));
        }
    }

    #[tokio::test]
    async fn initializing_one_path_does_not_block_another_queue() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().canonicalize().unwrap();
        let busy_path = parent.join("busy.db");
        let slot = open_slot(&busy_path).unwrap();
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let initializer = tokio::task::spawn_blocking(move || {
            let _slot = slot.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.blocking_recv();
        });
        locked_rx.await.unwrap();
        let waiting = tokio::spawn(Outbox::open(busy_path));
        let independent = tokio::time::timeout(
            Duration::from_secs(2),
            Outbox::open(parent.join("independent.db")),
        )
        .await;
        drop(release_tx);
        initializer.await.unwrap();
        waiting.await.unwrap().unwrap();
        independent
            .expect("another queue must open while the first path remains locked")
            .unwrap();
    }

    #[tokio::test]
    async fn retry_waits_until_due_and_rejects_stale_completion() {
        let (_directory, outbox) = queue().await;
        outbox
            .enqueue("event".into(), event(), vec![target("a")], 100)
            .await
            .unwrap();
        let first = outbox.claim_due(100, 1, vec![]).await.unwrap().remove(0);
        outbox
            .finish(
                &first,
                DeliveryOutcome::Retryable {
                    retry_after_ms: Some(10_000),
                },
                200,
            )
            .await
            .unwrap();
        assert_eq!(outbox.next_due_ms(vec![]).await.unwrap(), Some(10_200));
        assert!(
            outbox
                .claim_due(10_199, 1, vec![])
                .await
                .unwrap()
                .is_empty()
        );
        let second = outbox.claim_due(10_200, 1, vec![]).await.unwrap().remove(0);
        assert_eq!(second.attempt, 2);
        assert!(
            outbox
                .finish(&first, DeliveryOutcome::Delivered, 10_201)
                .await
                .is_err()
        );
        assert_eq!(state(&outbox, "a").await.0, "inflight");
        outbox
            .finish(&second, DeliveryOutcome::Uncertain, 10_201)
            .await
            .unwrap();
        assert_eq!(outbox.next_due_ms(vec![]).await.unwrap(), None);
    }

    #[test]
    fn backoff_is_bounded_and_honors_retry_after() {
        for attempt in 1..=100 {
            let delay = retry_delay_ms("delivery", attempt, None);
            assert!((1_000..=MAX_BACKOFF_MS).contains(&delay));
        }
        assert!((1_000..=1_250).contains(&retry_delay_ms("delivery", 1, None)));
        assert!((2_000..=2_500).contains(&retry_delay_ms("delivery", 2, None)));
        assert_eq!(retry_delay_ms("delivery", 3, Some(u64::MAX)), u64::MAX);
        assert_ne!(
            retry_delay_ms("first", 1, None),
            retry_delay_ms("second", 1, None)
        );
    }

    #[tokio::test]
    async fn concurrent_claims_return_a_delivery_only_once() {
        let (_directory, outbox) = queue().await;
        outbox
            .enqueue("event".into(), event(), vec![target("a")], 100)
            .await
            .unwrap();
        let mut claims = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let outbox = outbox.clone();
            claims.spawn(async move { outbox.claim_due(100, 10, vec![]).await.unwrap().len() });
        }
        let mut count = 0;
        while let Some(result) = claims.join_next().await {
            count += result.unwrap();
        }
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn claims_are_fair_and_serialize_each_channel() {
        let (_directory, outbox) = queue().await;
        for index in 0..5 {
            outbox
                .enqueue(
                    format!("event-{index}"),
                    event(),
                    vec![target("a"), target("b"), target("c")],
                    100,
                )
                .await
                .unwrap();
        }
        let first = outbox.claim_due(100, 1, vec![]).await.unwrap().remove(0);
        let others = outbox.claim_due(100, 10, vec![]).await.unwrap();
        assert_eq!(others.len(), 2);
        assert!(
            others
                .iter()
                .all(|delivery| delivery.target.channel_id != first.target.channel_id)
        );
        outbox
            .finish(&first, DeliveryOutcome::Delivered, 100)
            .await
            .unwrap();
        for delivery in &others {
            outbox
                .finish(delivery, DeliveryOutcome::Delivered, 100)
                .await
                .unwrap();
        }
        let mut served = HashSet::new();
        for _ in 0..3 {
            let delivery = outbox.claim_due(100, 1, vec![]).await.unwrap().remove(0);
            served.insert(delivery.target.channel_id.clone());
            outbox
                .finish(&delivery, DeliveryOutcome::Delivered, 100)
                .await
                .unwrap();
        }
        assert_eq!(served.len(), 3);
        assert_eq!(
            outbox
                .next_due_ms(vec!["a".into(), "b".into(), "c".into()])
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn retry_exhaustion_and_expiry_are_explicit_terminal_states() {
        let (_directory, outbox) = queue().await;
        outbox
            .enqueue("attempts".into(), event(), vec![target("a")], 100)
            .await
            .unwrap();
        let mut now = 100;
        for attempt in 1..=MAX_ATTEMPTS {
            let delivery = outbox.claim_due(now, 1, vec![]).await.unwrap().remove(0);
            assert_eq!(delivery.attempt, attempt);
            outbox
                .finish(
                    &delivery,
                    DeliveryOutcome::Retryable {
                        retry_after_ms: None,
                    },
                    now,
                )
                .await
                .unwrap();
            if attempt < MAX_ATTEMPTS {
                now = outbox.next_due_ms(vec![]).await.unwrap().unwrap();
            }
        }
        assert_eq!(
            state(&outbox, "a").await,
            (
                "permanent_failed".into(),
                MAX_ATTEMPTS,
                Some("attempt_limit".into())
            )
        );
        outbox
            .enqueue("expires".into(), event(), vec![target("b")], 100)
            .await
            .unwrap();
        assert!(
            outbox
                .claim_due(100 + DELIVERY_LIFETIME_MS, 10, vec![])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(state(&outbox, "b").await.2.as_deref(), Some("expired"));
    }

    #[tokio::test]
    async fn capacity_never_evicts_pending_and_retention_keeps_deduplication() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notifications.db");
        let outbox = tokio::task::spawn_blocking(move || {
            open_blocking(
                path,
                Limits {
                    pending: 1,
                    events: 2,
                    deliveries: 2,
                },
            )
        })
        .await
        .unwrap()
        .unwrap();
        outbox
            .enqueue("first".into(), event(), vec![target("a")], 100)
            .await
            .unwrap();
        assert!(
            outbox
                .enqueue("second".into(), event(), vec![target("b")], 100)
                .await
                .is_err()
        );
        assert!(!outbox.has_event("second".into()).await.unwrap());
        let delivery = outbox.claim_due(100, 1, vec![]).await.unwrap().remove(0);
        outbox
            .finish(&delivery, DeliveryOutcome::Cancelled, 100)
            .await
            .unwrap();
        outbox
            .enqueue("second".into(), event(), vec![], 101)
            .await
            .unwrap();
        assert!(
            outbox
                .enqueue("third".into(), event(), vec![], RETENTION_MS + 99)
                .await
                .is_err()
        );
        assert!(outbox.has_event("first".into()).await.unwrap());
        outbox
            .enqueue("third".into(), event(), vec![], RETENTION_MS + 100)
            .await
            .unwrap();
        assert!(!outbox.has_event("first".into()).await.unwrap());
        assert!(outbox.has_event("second".into()).await.unwrap());
    }

    #[tokio::test]
    async fn storage_excludes_credentials_and_raw_errors() {
        let (directory, outbox) = queue().await;
        let credential = "https://example.test/private-webhook-token";
        let hashed = DeliveryTarget {
            channel_id: "a".into(),
            binding_hash: crate::fs_util::sha256_hex_str(credential),
        };
        outbox
            .enqueue("event".into(), event(), vec![hashed], 100)
            .await
            .unwrap();
        assert!(
            outbox
                .enqueue(
                    "invalid".into(),
                    event(),
                    vec![DeliveryTarget {
                        channel_id: "b".into(),
                        binding_hash: credential.into()
                    }],
                    100
                )
                .await
                .is_err()
        );
        let path = directory.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            // 先关闭队列，释放 Windows 上会阻止文件读取的独占锁。
            drop(outbox);
            for entry in fs::read_dir(path).unwrap() {
                let bytes = fs::read(entry.unwrap().path()).unwrap();
                let content = String::from_utf8_lossy(&bytes);
                assert!(!content.contains(credential));
                assert!(!content.contains("private-raw-error-body"));
            }
        })
        .await
        .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn database_and_lock_files_are_private_and_symlinks_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (directory, _outbox) = queue().await;
        for name in ["notifications.db", "notifications.db.lock"] {
            assert_eq!(
                fs::metadata(directory.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let linked = directory.path().join("linked.db");
        symlink(directory.path().join("notifications.db"), &linked).unwrap();
        assert!(Outbox::open(linked).await.is_err());
    }

    #[tokio::test]
    async fn explicit_shutdown_marks_only_inflight_uncertain() {
        let (_directory, outbox) = queue().await;
        outbox
            .enqueue("event".into(), event(), vec![target("a"), target("b")], 100)
            .await
            .unwrap();
        let delivery = outbox
            .claim_due(100, 1, vec!["b".into()])
            .await
            .unwrap()
            .remove(0);
        outbox.mark_inflight_uncertain(101).await.unwrap();
        assert_eq!(state(&outbox, "a").await.0, "uncertain");
        assert_eq!(state(&outbox, "b").await.0, "pending");
        assert!(
            outbox
                .finish(&delivery, DeliveryOutcome::Delivered, 102)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_event_with_no_targets_cannot_gain_targets_later() {
        let (_directory, outbox) = queue().await;
        assert!(
            outbox
                .enqueue("event".into(), event(), vec![], 100)
                .await
                .unwrap()
        );
        assert!(
            !outbox
                .enqueue("event".into(), event(), vec![target("a")], 101)
                .await
                .unwrap()
        );
        assert!(outbox.claim_due(101, 1, vec![]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_claim_commit_never_returns_a_delivery() {
        let (_directory, outbox) = queue().await;
        outbox
            .enqueue("event".into(), event(), vec![target("a")], 100)
            .await
            .unwrap();
        outbox
            .run(|connection, _| {
                connection
                    .execute_batch("PRAGMA query_only = ON")
                    .map_err(db_error)
            })
            .await
            .unwrap();
        assert!(outbox.claim_due(100, 1, vec![]).await.is_err());
        assert_eq!(state(&outbox, "a").await.0, "pending");
    }

    #[tokio::test]
    async fn an_existing_process_lock_prevents_opening_the_queue() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notifications.db");
        let lock_path = directory.path().join("notifications.db.lock");
        let lock = tokio::task::spawn_blocking(move || {
            let lock = private_file(&lock_path).unwrap();
            fs2::FileExt::try_lock_exclusive(&lock).unwrap();
            lock
        })
        .await
        .unwrap();
        assert!(Outbox::open(path.clone()).await.is_err());
        tokio::task::spawn_blocking(move || drop(lock))
            .await
            .unwrap();
        assert!(Outbox::open(path).await.is_ok());
    }
}
