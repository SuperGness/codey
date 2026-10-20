use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};

mod delivery;

use super::AppState;
use crate::codex_config::codex_home;
use crate::config::{CodeyConfig, ConfigStore};
use crate::notifications::{
    NotificationChannelConfig, NotificationChannelKind, NotificationDispatcher, NotificationEvent,
};
use crate::pending_approval;
use crate::pending_approval::{CompletedTurn, RecentSessionEvents, SessionLifecycleStatus};
use crate::{local_router, model_id};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaitingNotificationLedger {
    #[serde(default)]
    notification_keys: Vec<String>,
}

const WEBHOOK_NOTIFICATION_HISTORY_LIMIT: usize = 2048;
const MAX_CONCURRENT_WEBHOOK_DELIVERIES: usize = 4;
const NOTIFICATION_TITLE_BUDGET: Duration = Duration::from_millis(200);
const NOTIFICATION_TITLE_CACHE_LIMIT: usize = 256;
const SLOW_SESSION_SCAN: Duration = Duration::from_millis(500);
const WEBHOOK_TURN_HISTORY_LIMIT: usize =
    pending_approval::MAX_RECENT_SESSIONS * pending_approval::MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT;

/// 只保存经过 metadata 标题清理及占位内容过滤的结果。
#[derive(Default)]
pub(super) struct NotificationTitleCache {
    titles: HashMap<String, String>,
    order: VecDeque<String>,
}

impl NotificationTitleCache {
    fn get(&self, session_id: &str) -> String {
        self.titles
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| "未命名会话".to_string())
    }

    fn insert_resolved(&mut self, session_id: String, title: String) {
        if !self.titles.contains_key(&session_id) {
            self.order.push_back(session_id.clone());
        }
        self.titles.insert(session_id, title);
        while self.order.len() > NOTIFICATION_TITLE_CACHE_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.titles.remove(&oldest);
            }
        }
    }
}

fn notification_observation(_category: &'static str, _fields: Value) {
    #[cfg(not(test))]
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(_category, _fields);
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[derive(Default)]
pub(super) struct WaitingNotificationSnapshot {
    known_sessions: HashSet<String>,
    active_keys: HashSet<String>,
}

impl WaitingNotificationSnapshot {
    fn from_events(events: &RecentSessionEvents) -> Self {
        Self {
            known_sessions: events.session_statuses.keys().cloned().collect(),
            active_keys: waiting_notification_keys(&events.pending_approvals),
        }
    }

    fn is_obsolete(&self, session_id: &str, event_key: &str) -> bool {
        // 扫描失败或会话超出索引范围时，缺少记录不能证明提问已结束。
        self.known_sessions.contains(session_id) && !self.active_keys.contains(event_key)
    }
}

fn waiting_notification_ledger_path(store: &ConfigStore) -> PathBuf {
    store.path().with_file_name("webhook-notifications.json")
}

/// Persisted "waiting" reservation keys in insertion order so the ledger stays
/// bounded like the in-memory settled history: once the cap is exceeded the
/// oldest reservations fall off. Evicting a key that old only weakens dedup
/// toward the documented at-most-once boundary (a suppressed resend can become
/// possible again), never toward duplicate sends of recent notifications.
#[derive(Debug, Default)]
pub(super) struct WaitingLedgerState {
    keys: HashSet<String>,
    order: VecDeque<String>,
    revision: u64,
    io: Arc<Mutex<u64>>,
}

impl WaitingLedgerState {
    fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    #[cfg(test)]
    pub(super) fn iter(&self) -> impl Iterator<Item = &String> {
        self.order.iter()
    }

    fn ordered_keys(&self) -> Vec<String> {
        self.order.iter().cloned().collect()
    }

    fn insert(&mut self, key: String) -> bool {
        if !self.keys.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > WEBHOOK_NOTIFICATION_HISTORY_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.keys.remove(&oldest);
            }
        }
        self.revision += 1;
        true
    }

    fn extend(&mut self, keys: impl IntoIterator<Item = String>) -> bool {
        let mut changed = false;
        for key in keys {
            changed |= self.insert(key);
        }
        changed
    }
}

fn load_waiting_notification_entries(path: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(ledger) = serde_json::from_str::<WaitingNotificationLedger>(&contents) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    ledger
        .notification_keys
        .into_iter()
        .filter(|key| key.starts_with("waiting:") && seen.insert(key.clone()))
        .collect()
}

#[cfg(test)]
fn load_waiting_notification_ledger(path: &Path) -> HashSet<String> {
    load_waiting_notification_entries(path)
        .into_iter()
        .collect()
}

fn waiting_notification_key(session_id: &str, turn_id: &str, waiting_id: &str) -> String {
    format!("waiting:{session_id}:{turn_id}:{waiting_id}")
}

fn terminal_turn_key(session_id: &str, turn_id: &str) -> String {
    format!(
        "{}:{}",
        session_id.trim().trim_start_matches("local:"),
        turn_id.trim()
    )
}

#[derive(Debug, Default)]
struct WebhookTurnTracker {
    running: HashSet<String>,
    settled: HashSet<String>,
    settled_order: VecDeque<String>,
    ignore_completed_before: i64,
}

type RecentSessionScanResult = (
    pending_approval::RecentSessionEventCache,
    Arc<RecentSessionEvents>,
);
pub(super) struct RecentSessionScanTask {
    task: tokio::task::JoinHandle<(RecentSessionScanResult, Duration)>,
    started_at: i64,
}

impl RecentSessionScanTask {
    // 计时放在阻塞任务内：启动期的初始扫描要等 Codex 就绪后才被 await。
    fn spawn(scan: impl FnOnce() -> RecentSessionScanResult + Send + 'static) -> Self {
        Self {
            started_at: unix_timestamp_seconds(),
            task: tokio::task::spawn_blocking(move || {
                let started = Instant::now();
                let result = scan();
                (result, started.elapsed())
            }),
        }
    }
}

const ACTIVE_SESSION_SCAN_DELAY: Duration = Duration::from_secs(3);
const IDLE_SESSION_SCAN_DELAYS: [Duration; 4] = [
    Duration::from_secs(3),
    Duration::from_secs(6),
    Duration::from_secs(12),
    Duration::from_secs(30),
];

#[derive(Debug, Default)]
struct SessionScanSchedule {
    idle_delay_index: usize,
}

impl SessionScanSchedule {
    fn wake(&mut self) {
        self.idle_delay_index = 0;
    }

    fn delay_after_scan(
        &mut self,
        previous: &RecentSessionEvents,
        current: &RecentSessionEvents,
    ) -> Duration {
        if session_events_are_active(current) || session_event_state_changed(previous, current) {
            self.wake();
            return ACTIVE_SESSION_SCAN_DELAY;
        }

        let delay = IDLE_SESSION_SCAN_DELAYS[self.idle_delay_index];
        self.idle_delay_index = (self.idle_delay_index + 1).min(IDLE_SESSION_SCAN_DELAYS.len() - 1);
        delay
    }
}

fn session_events_are_active(events: &RecentSessionEvents) -> bool {
    !events.pending_approvals.is_empty()
        || events.session_statuses.values().any(|status| {
            matches!(
                status,
                SessionLifecycleStatus::Running | SessionLifecycleStatus::Waiting
            )
        })
}

fn session_event_state_changed(
    previous: &RecentSessionEvents,
    current: &RecentSessionEvents,
) -> bool {
    previous.session_statuses != current.session_statuses
        || previous.pending_approvals.len() != current.pending_approvals.len()
        || previous
            .pending_approvals
            .iter()
            .zip(&current.pending_approvals)
            .any(|(left, right)| {
                left.session_id != right.session_id
                    || left.turn_id != right.turn_id
                    || left.waiting_id != right.waiting_id
            })
        || previous.started_turns.len() != current.started_turns.len()
        || previous
            .started_turns
            .iter()
            .zip(current.started_turns.iter())
            .any(|(left, right)| {
                left.session_id != right.session_id || left.turn_id != right.turn_id
            })
        || previous.aborted_turns.len() != current.aborted_turns.len()
        || previous
            .aborted_turns
            .iter()
            .zip(current.aborted_turns.iter())
            .any(|(left, right)| {
                left.session_id != right.session_id
                    || left.turn_id != right.turn_id
                    || left.is_snapshot_replay != right.is_snapshot_replay
            })
        || previous.completed_turns.len() != current.completed_turns.len()
        || previous
            .completed_turns
            .iter()
            .zip(current.completed_turns.iter())
            .any(|(left, right)| {
                left.session_id != right.session_id
                    || left.turn_id != right.turn_id
                    || left.completed_at != right.completed_at
                    || left.error != right.error
                    || left.is_snapshot_replay != right.is_snapshot_replay
            })
}

impl WebhookTurnTracker {
    #[cfg(test)]
    fn from_snapshot(events: &RecentSessionEvents) -> Self {
        Self::from_snapshot_at(events, unix_timestamp_seconds())
    }

    fn from_snapshot_at(events: &RecentSessionEvents, observed_at: i64) -> Self {
        let mut tracker = Self {
            ignore_completed_before: observed_at,
            ..Self::default()
        };
        for started in events.started_turns.iter() {
            tracker
                .running
                .insert(terminal_turn_key(&started.session_id, &started.turn_id));
        }
        for completed in events.completed_turns.iter() {
            tracker.mark_settled(completed);
        }
        for aborted in events.aborted_turns.iter() {
            tracker.mark_terminal(&aborted.session_id, &aborted.turn_id);
        }
        tracker
    }

    fn completion_candidates(&mut self, events: &RecentSessionEvents) -> Vec<CompletedTurn> {
        for started in events.started_turns.iter() {
            let key = terminal_turn_key(&started.session_id, &started.turn_id);
            if !self.settled.contains(&key) {
                self.running.insert(key);
            }
        }
        for aborted in events.aborted_turns.iter() {
            self.mark_terminal(&aborted.session_id, &aborted.turn_id);
        }

        let mut candidates = Vec::new();
        let mut candidate_keys = HashSet::new();
        for completed in events.completed_turns.iter() {
            let key = terminal_turn_key(&completed.session_id, &completed.turn_id);
            if self.settled.contains(&key) || !candidate_keys.insert(key.clone()) {
                continue;
            }
            if completed.is_snapshot_replay
                || completed
                    .completed_at
                    .is_some_and(|completed_at| completed_at < self.ignore_completed_before)
            {
                self.mark_terminal(&completed.session_id, &completed.turn_id);
                continue;
            }
            if self.running.contains(&key) {
                candidates.push(completed.clone());
            } else {
                // A terminal event without a running edge belongs to snapshot history.
                self.remember_terminal_key(key);
            }
        }
        candidates
    }

    fn mark_settled(&mut self, completed: &CompletedTurn) {
        self.mark_terminal(&completed.session_id, &completed.turn_id);
    }

    fn mark_terminal(&mut self, session_id: &str, turn_id: &str) {
        let key = terminal_turn_key(session_id, turn_id);
        self.running.remove(&key);
        self.remember_terminal_key(key);
    }

    fn remember_terminal_key(&mut self, key: String) {
        if self.settled.insert(key.clone()) {
            self.settled_order.push_back(key);
        }
        while self.settled_order.len() > WEBHOOK_TURN_HISTORY_LIMIT {
            if let Some(oldest) = self.settled_order.pop_front() {
                self.settled.remove(&oldest);
            }
        }
    }
}

fn unix_timestamp_seconds() -> i64 {
    crate::fs_util::timestamp_secs() as i64
}

fn initialize_waiting_notifications(path: &Path, baseline: HashSet<String>) -> WaitingLedgerState {
    let path_existed = path.exists();
    let mut initialized = WaitingLedgerState::default();
    let mut loaded_matches_file = true;
    if path_existed {
        let entries = load_waiting_notification_entries(path);
        let entry_count = entries.len();
        initialized.extend(entries);
        loaded_matches_file = initialized.order.len() == entry_count;
    }
    let baseline_changed = initialized.extend(baseline);
    if !initialized.is_empty() && (!path_existed || !loaded_matches_file || baseline_changed) {
        let _ = save_waiting_notification_ledger(path, initialized.ordered_keys());
    }
    initialized
}

pub(super) fn initial_waiting_notifications(
    store: &ConfigStore,
    pending_approvals: &[pending_approval::PendingApproval],
) -> WaitingLedgerState {
    let path = waiting_notification_ledger_path(store);
    initialize_waiting_notifications(&path, waiting_notification_keys(pending_approvals))
}

fn waiting_notification_keys(
    pending_approvals: &[pending_approval::PendingApproval],
) -> HashSet<String> {
    pending_approvals
        .iter()
        .map(|pending| {
            waiting_notification_key(
                pending.session_id.trim_start_matches("local:"),
                &pending.turn_id,
                &pending.waiting_id,
            )
        })
        .collect()
}

fn save_waiting_notification_ledger(
    path: &Path,
    notification_keys: Vec<String>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "消息通知记录路径无父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建消息通知记录目录失败：{error}"))?;
    let bytes = serde_json::to_vec_pretty(&WaitingNotificationLedger { notification_keys })
        .map_err(|error| format!("序列化消息通知记录失败：{error}"))?;
    crate::fs_util::atomic_write_private(path, &bytes)
        .map_err(|error| format!("替换消息通知记录失败：{error}"))?;
    Ok(())
}

/// Writes the current ledger snapshot to disk without holding the state lock
/// across IO. The write itself runs on a blocking thread; the per-ledger `io`
/// mutex serializes writers, and the revision check skips writes that a later
/// snapshot already covered (a higher revision is always a more recent state).
async fn persist_waiting_ledger(state: &Arc<AppState>) -> Result<(), String> {
    let io = {
        let ledger = state.persisted_waiting_notifications.lock().await;
        ledger.io.clone()
    };
    let mut written = io.lock().await;
    let (snapshot, revision) = {
        let ledger = state.persisted_waiting_notifications.lock().await;
        if ledger.revision <= *written {
            return Ok(());
        }
        (ledger.ordered_keys(), ledger.revision)
    };
    let path = waiting_notification_ledger_path(&state.store);
    tokio::task::spawn_blocking(move || save_waiting_notification_ledger(&path, snapshot))
        .await
        .map_err(|error| format!("写入消息通知记录任务失败：{error}"))??;
    *written = revision;
    Ok(())
}

async fn baseline_waiting_notifications(
    state: &Arc<AppState>,
    pending_approvals: &[pending_approval::PendingApproval],
) {
    let baseline = waiting_notification_keys(pending_approvals);
    if baseline.is_empty() {
        return;
    }

    let changed = {
        let mut persisted = state.persisted_waiting_notifications.lock().await;
        persisted.extend(baseline.iter().cloned())
    };
    if changed {
        let _ = persist_waiting_ledger(state).await;
    }
}
pub(super) fn webhook_watcher_should_run(config: &CodeyConfig) -> bool {
    config.webhook.has_enabled_channel()
}

pub(super) async fn sync_waiting_webhook_watcher(state: &Arc<AppState>) {
    let _sync_guard = state.waiting_watcher_sync.lock().await;
    let should_run = {
        let config = state.config.read().await;
        webhook_watcher_should_run(&config)
    };
    let runtime_running = state.runtime.lock().await.is_some();
    if runtime_running && should_run {
        start_waiting_webhook_watcher_from_cache(state).await;
    } else {
        stop_waiting_webhook_watcher(state).await;
    }
}

async fn start_waiting_webhook_watcher_from_cache(state: &Arc<AppState>) {
    if state.waiting_watcher_task.lock().await.is_some() {
        return;
    }
    let initial_event_cache = state
        .recent_session_event_cache
        .lock()
        .await
        .take()
        .unwrap_or_default();
    let initial_scan_task = start_recent_session_scan(initial_event_cache);
    start_waiting_webhook_watcher(state, initial_scan_task).await;
}

pub(super) async fn start_waiting_webhook_watcher(
    state: &Arc<AppState>,
    initial_scan_task: RecentSessionScanTask,
) {
    state.notification_delivery_stop.send_replace(false);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let watcher_state = Arc::clone(state);
    let watcher_task = tokio::spawn(async move {
        let home = notification_codex_home(&watcher_state);
        let file_watcher = pending_approval::file_events::SessionFileWatcher::start(
            home.clone(),
            Arc::clone(&watcher_state.session_scan_wake),
        )
        .ok();
        let mut was_healthy = file_watcher
            .as_ref()
            .is_some_and(|watcher| watcher.is_healthy());
        notification_observation(
            "notification.watcher_started",
            json!({
                "nativeWatcher": was_healthy,
                "enabledChannels": watcher_state.config.read().await.webhook.channels.iter()
                    .filter(|channel| channel.enabled && channel.is_configured()).count(),
            }),
        );
        if !was_healthy {
            notification_observation(
                "notification.watcher_degraded",
                json!({"reason":"initialization_failed"}),
            );
        }
        let baseline_started_at = initial_scan_task.started_at;
        let (mut event_cache, baseline_events) = await_recent_session_scan(initial_scan_task).await;
        if let Some(watcher) = &file_watcher {
            watcher.sync_cache(&event_cache, true);
        }
        if !matches!(
            shutdown_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ) {
            event_cache.release_database_connections();
            *watcher_state.recent_session_event_cache.lock().await = Some(event_cache);
            return;
        }
        baseline_waiting_notifications(&watcher_state, &baseline_events.pending_approvals).await;
        let mut queued_waiting = waiting_notification_keys(&baseline_events.pending_approvals);
        // 先完成注册监听后的补查，再恢复队列，避免按预热期间的旧状态发送等待提示。
        let mut worker: Option<tokio::task::JoinHandle<()>> = None;
        let mut turn_tracker =
            WebhookTurnTracker::from_snapshot_at(&baseline_events, baseline_started_at);
        let mut previous_events = baseline_events;
        let mut scan_schedule = SessionScanSchedule::default();
        let mut next_scan_delay = ACTIVE_SESSION_SCAN_DELAY;
        let mut next_full_scan = tokio::time::Instant::now();
        let mut scan_immediately = true;
        loop {
            if !scan_immediately {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    _ = watcher_state.session_scan_wake.notified() => { scan_schedule.wake(); },
                    _ = tokio::time::sleep(next_scan_delay) => {},
                }
            }
            scan_immediately = false;
            if *watcher_state.notification_delivery_stop.borrow() {
                break;
            }
            if worker.as_ref().is_some_and(|worker| worker.is_finished()) {
                if let Some(finished) = worker.take() {
                    let _ = finished.await;
                }
                if let Ok(queue) = delivery::outbox(&watcher_state).await {
                    let _ = queue.mark_inflight_uncertain(delivery::now_ms()).await;
                }
            }
            let healthy = file_watcher
                .as_ref()
                .is_some_and(|watcher| watcher.is_healthy());
            if was_healthy && !healthy {
                notification_observation(
                    "notification.watcher_degraded",
                    json!({"reason":"native_watcher_failed"}),
                );
            }
            was_healthy = healthy;
            let changes = file_watcher.as_ref().map(|watcher| watcher.take_changes());
            let full_scan = !healthy || tokio::time::Instant::now() >= next_full_scan;
            let scan_started = Instant::now();
            let (next_cache, events) = if full_scan {
                next_full_scan = tokio::time::Instant::now() + Duration::from_secs(60);
                scan_recent_session_events(event_cache, home.clone()).await
            } else if let Some(changes) = changes {
                if changes.is_empty() {
                    // 页面和其他外部提示没有文件路径，也必须能够主动补查。
                    next_full_scan = tokio::time::Instant::now() + Duration::from_secs(60);
                    scan_recent_session_events(event_cache, home.clone()).await
                } else {
                    let home = home.clone();
                    await_recent_session_scan(RecentSessionScanTask::spawn(move || {
                        let events = event_cache.refresh_changes(&home, changes);
                        (event_cache, events)
                    }))
                    .await
                }
            } else {
                unreachable!("healthy watcher has a change buffer")
            };
            let scan_ms = elapsed_ms(scan_started);
            event_cache = next_cache;
            if let Some(watcher) = &file_watcher {
                watcher.sync_cache(&event_cache, full_scan);
            }
            if *watcher_state.notification_delivery_stop.borrow() {
                break;
            }
            *watcher_state.notification_waiting_snapshot.write().await =
                WaitingNotificationSnapshot::from_events(&events);
            if worker.is_none() {
                worker = delivery::start(Arc::clone(&watcher_state)).await.ok();
            }
            // 扫描只等待本地入队，远端响应和重试由独立发送任务处理。
            let mut enqueue_pending =
                !notify_pending_approvals(&watcher_state, &events, &mut queued_waiting, scan_ms)
                    .await;
            for completed in turn_tracker.completion_candidates(&events) {
                if *watcher_state.notification_delivery_stop.borrow() {
                    break;
                }
                let (model, reasoning_effort) =
                    webhook_turn_configuration(&events, &completed.session_id, &completed.turn_id);
                let payload = json!({
                    "sessionId": completed.session_id, "turnId": completed.turn_id,
                    "durationMs": completed.duration_ms, "rolloutError": completed.error,
                    "confirmedByRollout": true, "model": model, "reasoningEffort": reasoning_effort,
                    "scanMs": scan_ms,
                });
                match notify_webhook_completion(&watcher_state, &payload).await {
                    Ok(_) => turn_tracker.mark_settled(&completed),
                    Err(_) => enqueue_pending = true,
                }
            }
            next_scan_delay = if enqueue_pending || worker.is_none() {
                notification_observation(
                    "notification.enqueue_error",
                    json!({"retryScheduled":true}),
                );
                ACTIVE_SESSION_SCAN_DELAY
            } else if healthy {
                next_full_scan.saturating_duration_since(tokio::time::Instant::now())
            } else {
                scan_schedule.delay_after_scan(&previous_events, &events)
            };
            previous_events = events;
        }
        drop(file_watcher);
        watcher_state.notification_delivery_stop.send_replace(true);
        if let Some(worker) = worker {
            let _ = worker.await;
        }
        event_cache.release_database_connections();
        *watcher_state.recent_session_event_cache.lock().await = Some(event_cache);
    });
    *state.waiting_watcher_shutdown.lock().await = Some(shutdown_tx);
    *state.waiting_watcher_task.lock().await = Some(watcher_task);
}

async fn notify_pending_approvals(
    state: &Arc<AppState>,
    events: &RecentSessionEvents,
    queued_keys: &mut HashSet<String>,
    scan_ms: u64,
) -> bool {
    let active_keys = waiting_notification_keys(&events.pending_approvals);
    queued_keys.retain(|key| active_keys.contains(key));
    let mut queued = true;
    for pending in &events.pending_approvals {
        let key =
            waiting_notification_key(&pending.session_id, &pending.turn_id, &pending.waiting_id);
        if queued_keys.contains(&key) {
            continue;
        }
        let (model, reasoning_effort) =
            webhook_turn_configuration(events, &pending.session_id, &pending.turn_id);
        let payload = json!({
            "sessionId": pending.session_id,
            "turnId": pending.turn_id,
            "waitingId": pending.waiting_id,
            "durationMs": pending.duration_ms,
            "model": model,
            "reasoningEffort": reasoning_effort,
            "scanMs": scan_ms,
        });
        if notify_webhook_waiting(state, &payload).await.is_err() {
            queued = false;
        } else {
            queued_keys.insert(key);
        }
    }
    queued
}

async fn scan_recent_session_events(
    cache: pending_approval::RecentSessionEventCache,
    home: PathBuf,
) -> RecentSessionScanResult {
    await_recent_session_scan(start_recent_session_scan_at(cache, home)).await
}

pub(super) fn start_recent_session_scan(
    cache: pending_approval::RecentSessionEventCache,
) -> RecentSessionScanTask {
    start_recent_session_scan_at(cache, codex_home().to_path_buf())
}

fn start_recent_session_scan_at(
    mut cache: pending_approval::RecentSessionEventCache,
    home: PathBuf,
) -> RecentSessionScanTask {
    RecentSessionScanTask::spawn(move || {
        let events = cache.refresh(&home);
        (cache, events)
    })
}

fn notification_codex_home(_state: &AppState) -> PathBuf {
    #[cfg(test)]
    if let Some(home) = &_state.notification_codex_home {
        return home.clone();
    }
    codex_home().to_path_buf()
}

pub(super) async fn await_recent_session_scan(
    scan_task: RecentSessionScanTask,
) -> RecentSessionScanResult {
    match scan_task.task.await {
        Ok((result, scan_time)) => {
            if scan_time >= SLOW_SESSION_SCAN {
                notification_observation(
                    "notification.scan_slow",
                    json!({
                        "scanMs":scan_time.as_millis() as u64,
                        "sessions":result.1.session_statuses.len(),
                        "waiting":result.1.pending_approvals.len(),
                        "completed":result.1.completed_turns.len(),
                    }),
                );
            }
            result
        }
        Err(_) => {
            notification_observation("notification.scan_failed", json!({"reason":"task_failed"}));
            (
                pending_approval::RecentSessionEventCache::default(),
                Arc::new(RecentSessionEvents::default()),
            )
        }
    }
}

pub(super) async fn stop_waiting_webhook_watcher(state: &Arc<AppState>) {
    state.notification_delivery_stop.send_replace(true);
    let shutdown = state.waiting_watcher_shutdown.lock().await.take();
    if let Some(shutdown) = shutdown {
        let _ = shutdown.send(());
    }
    let watcher_task = state.waiting_watcher_task.lock().await.take();
    if let Some(watcher_task) = watcher_task
        && watcher_task.await.is_err()
    {
        notification_observation(
            "notification.watcher_failed",
            json!({"reason":"task_failed"}),
        );
    }
    let mut event_cache = state.recent_session_event_cache.lock().await;
    if event_cache.is_none() {
        *event_cache = Some(pending_approval::RecentSessionEventCache::default());
    }
}

pub(super) async fn test_notification_channel(
    state: &Arc<AppState>,
    channel: NotificationChannelConfig,
) -> Result<Value, String> {
    // Renderer drafts never receive saved webhook URLs or bot tokens. Restore
    // only the matching channel's redacted value for this one test request,
    // using the same explicit-clear semantics as the save path.
    let previous = state.config.read().await.webhook.clone();
    let mut draft = crate::notifications::WebhookConfig {
        channels: vec![channel],
        ..crate::notifications::WebhookConfig::default()
    };
    draft.merge_redacted_secrets(&previous);
    let channel = draft
        .channels
        .pop()
        .expect("notification test draft must contain one channel");
    let client = current_notification_http_client(state)?;
    let dispatcher = NotificationDispatcher::with_client(client, channel);
    dispatcher.test().await.map_err(|error| error.to_string())
}

fn current_notification_http_client(_state: &AppState) -> Result<reqwest::Client, String> {
    #[cfg(test)]
    if let Some(client) = &_state.webhook_http_client_override {
        return Ok(client.clone());
    }

    crate::notifications::shared_notification_http_client().map_err(|error| error.to_string())
}

fn webhook_session_configuration(
    requested_model: &str,
    requested_reasoning_effort: &str,
) -> (String, String) {
    let model = if requested_model.trim().is_empty() {
        "Codex".to_string()
    } else {
        requested_model.trim().to_string()
    };
    let reasoning_effort = if requested_reasoning_effort.trim().is_empty() {
        "默认".to_string()
    } else {
        requested_reasoning_effort.trim().to_string()
    };
    (model, reasoning_effort)
}

fn webhook_display_model(config: &CodeyConfig, requested_model: &str) -> String {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return "Codex".to_string();
    }

    let mut matching_raw_profile = None;
    for profile in &config.profiles {
        let provider_id = profile.provider_id().trim();
        if provider_id.is_empty() {
            continue;
        }

        let alias_prefix = local_router::model_alias(provider_id, "");
        if let Some(model) = requested_model
            .strip_prefix(&alias_prefix)
            .map(str::trim)
            .filter(|model| !model.is_empty())
        {
            return format_webhook_model_name(profile, model);
        }

        let enabled_models = if profile.official_account {
            config.enabled_official_route_models(provider_id)
        } else {
            config.enabled_route_models(provider_id)
        };
        if enabled_models
            .iter()
            .any(|model| model_id::equal(model, requested_model))
        {
            if matching_raw_profile.is_some() {
                // A raw model shared by multiple routes has no reliable route
                // identity, so retain it instead of showing a misleading prefix.
                matching_raw_profile = None;
                break;
            }
            matching_raw_profile = Some(profile);
        }
    }

    matching_raw_profile
        .map(|profile| format_webhook_model_name(profile, requested_model))
        .unwrap_or_else(|| requested_model.to_string())
}

fn format_webhook_model_name(profile: &crate::config::ProviderProfile, model: &str) -> String {
    let prefix = profile.display_short_name();
    if prefix.is_empty() {
        model.to_string()
    } else {
        format!("[{prefix}] {model}")
    }
}

fn webhook_turn_configuration(
    events: &RecentSessionEvents,
    session_id: &str,
    turn_id: &str,
) -> (String, String) {
    let Some(configuration) = events
        .turn_configurations
        .get(session_id)
        .and_then(|turns| turns.get(turn_id))
    else {
        return webhook_session_configuration("", "");
    };
    webhook_session_configuration(&configuration.model, &configuration.reasoning_effort)
}
async fn webhook_session_name(state: &Arc<AppState>, session_id: &str) -> String {
    let fallback = state
        .notification_title_cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id);
    let started = Instant::now();
    let query = async {
        let permit = Arc::clone(&state.notification_title_query)
            .try_acquire_owned()
            .map_err(|_| "query_busy")?;
        // 标题只取侧边栏同步和会话元数据，不采信通知载荷中的名称。
        let preferred_title = state.session_titles.read().await.get(session_id).cloned();
        let home = notification_codex_home(state);
        let session_id = session_id.to_string();
        let state = Arc::clone(state);
        tokio::task::spawn_blocking(move || {
            // 超时不能取消已启动的阻塞查询，许可须保留到闭包退出。
            let _permit = permit;
            let mut metadata = match state.session_metadata_cache.try_lock() {
                Ok(metadata) => metadata,
                Err(std::sync::TryLockError::WouldBlock) => return Err("metadata_busy"),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            let title = metadata.resolve_session_name_with_preferred(
                &home,
                &session_id,
                preferred_title.as_deref(),
            );
            state
                .notification_title_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert_resolved(session_id, title.clone());
            Ok(title)
        })
        .await
        .map_err(|_| "query_failed")?
    };
    let reason = match tokio::time::timeout(NOTIFICATION_TITLE_BUDGET, query).await {
        Ok(Ok(title)) => return title,
        Ok(Err(reason)) => reason,
        Err(_) => "query_timeout",
    };
    notification_observation(
        "notification.title_fallback",
        json!({"reason": reason, "titleMs": elapsed_ms(started)}),
    );
    fallback
}

fn observe_notification_candidate(
    key: &str,
    kind: &'static str,
    payload: &Value,
    title_ms: u64,
    enqueue_started: Instant,
    result: &Result<Value, String>,
) {
    let outcome = match result {
        Ok(value) if value.get("status").and_then(Value::as_str) == Some("queued") => "queued",
        Ok(_) => "duplicate",
        Err(_) => "enqueue_failed",
    };
    notification_observation(
        "notification.candidate",
        json!({
            "eventHash": crate::fs_util::sha256_hex_str(key),
            "kind": kind,
            "outcome": outcome,
            "scanMs": payload.get("scanMs").and_then(Value::as_u64).unwrap_or_default(),
            "titleMs": title_ms,
            "enqueueMs": elapsed_ms(enqueue_started),
        }),
    );
}

async fn notify_webhook_completion(
    state: &Arc<AppState>,
    payload: &Value,
) -> Result<Value, String> {
    let session_id = payload
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .trim_start_matches("local:")
        .to_string();
    let turn_id = payload
        .get("turnId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if session_id.is_empty() || turn_id.is_empty() {
        return Err("任务完成通知缺少会话或轮次 ID".to_string());
    }
    let confirmed_by_rollout = payload
        .get("confirmedByRollout")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !confirmed_by_rollout {
        return Ok(json!({
            "status":"skipped",
            "reason":"awaiting-authoritative-rollout-terminal"
        }));
    }
    let requested_duration_ms = payload
        .get("durationMs")
        .and_then(Value::as_u64)
        .unwrap_or_default() as u128;
    let rollout_error = payload
        .get("rolloutError")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|error| !error.is_empty())
        .map(ToString::to_string);
    let requested_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let requested_reasoning_effort = payload
        .get("reasoningEffort")
        .or_else(|| payload.get("reasoning_effort"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (model, reasoning_effort) =
        webhook_session_configuration(requested_model, requested_reasoning_effort);
    let (channels, profile_id, model) = {
        let config = state.config.read().await;
        (
            config.webhook.channels.clone(),
            config
                .active_profile()
                .map(|profile| profile.id)
                .unwrap_or_default(),
            webhook_display_model(&config, &model),
        )
    };
    if !channels
        .iter()
        .any(|channel| channel.enabled && channel.is_configured())
    {
        return Ok(json!({"status":"skipped","reason":"disabled"}));
    }
    let key = format!("terminal:{session_id}:{turn_id}");
    if delivery::outbox(state)
        .await?
        .has_event(key.clone())
        .await?
    {
        return Ok(json!({"status":"duplicate"}));
    }
    let title_started = Instant::now();
    let session_name = webhook_session_name(state, &session_id).await;
    let title_ms = elapsed_ms(title_started);
    let enqueue_started = Instant::now();
    let kind = if rollout_error.is_some() {
        "session.failed"
    } else {
        "session.completed"
    };
    let event = NotificationEvent::new(
        kind,
        session_id,
        profile_id,
        model,
        requested_duration_ms,
        rollout_error,
    )
    .with_session_name(session_name)
    .with_reasoning_effort(reasoning_effort);
    let result = delivery::enqueue(state, key.clone(), event, channels).await;
    observe_notification_candidate(&key, kind, payload, title_ms, enqueue_started, &result);
    result
}

async fn notify_webhook_waiting(state: &Arc<AppState>, payload: &Value) -> Result<Value, String> {
    let session_id = payload
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .trim_start_matches("local:")
        .to_string();
    if session_id.is_empty() {
        return Err("等待介入通知缺少会话 ID".to_string());
    }
    let turn_id = payload
        .get("turnId")
        .and_then(Value::as_str)
        .unwrap_or("active")
        .trim();
    let waiting_id = payload
        .get("waitingId")
        .and_then(Value::as_str)
        .unwrap_or("waiting")
        .trim();
    let duration_ms = payload
        .get("durationMs")
        .and_then(Value::as_u64)
        .unwrap_or_default() as u128;
    let requested_model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let requested_reasoning_effort = payload
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (model, reasoning_effort) =
        webhook_session_configuration(requested_model, requested_reasoning_effort);
    let (channels, profile_id, model) = {
        let config = state.config.read().await;
        (
            config.webhook.channels.clone(),
            config
                .active_profile()
                .map(|profile| profile.id)
                .unwrap_or_default(),
            webhook_display_model(&config, &model),
        )
    };
    if !channels
        .iter()
        .any(|channel| channel.enabled && channel.is_configured())
    {
        return Ok(json!({"status":"skipped","reason":"disabled"}));
    }

    let notification_key = waiting_notification_key(&session_id, turn_id, waiting_id);
    // 旧账本只作为历史去重记录，新的等待通知由 outbox 原子入队。
    if state
        .persisted_waiting_notifications
        .lock()
        .await
        .contains(&notification_key)
        || delivery::outbox(state)
            .await?
            .has_event(notification_key.clone())
            .await?
    {
        return Ok(json!({"status":"duplicate"}));
    }
    let title_started = Instant::now();
    let session_name = webhook_session_name(state, &session_id).await;
    let title_ms = elapsed_ms(title_started);
    let enqueue_started = Instant::now();
    let event = NotificationEvent::new(
        "session.waiting",
        session_id,
        profile_id,
        model,
        duration_ms,
        None,
    )
    .with_session_name(session_name)
    .with_reasoning_effort(reasoning_effort);
    let result = delivery::enqueue(state, notification_key.clone(), event, channels).await;
    observe_notification_candidate(
        &notification_key,
        "session.waiting",
        payload,
        title_ms,
        enqueue_started,
        &result,
    );
    result
}
#[cfg(test)]
mod tests {
    include!("webhooks/observation_tests.rs");
    include!("webhooks/runtime_tests.rs");
    use std::collections::{HashMap, HashSet};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use tokio::sync::{Mutex, RwLock, Semaphore, oneshot};

    use super::*;
    use crate::config::ConfigStore;
    use crate::pending_approval::{AbortedTurn, PendingApproval, StartedTurn};

    async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;

        let mut request = Vec::new();
        let mut body_start = None;
        let mut content_length = 0_usize;
        loop {
            let mut chunk = [0_u8; 1024];
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if body_start.is_none()
                && let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n")
            {
                body_start = Some(index + 4);
                let headers = String::from_utf8_lossy(&request[..index]);
                content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
            }
            if let Some(body_start) = body_start
                && request.len() >= body_start + content_length
            {
                break;
            }
        }
        String::from_utf8(request).unwrap()
    }

    async fn write_json_response(socket: &mut tokio::net::TcpStream, body: &str) {
        use tokio::io::AsyncWriteExt;

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    }

    struct MockWebhookServer {
        url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl MockWebhookServer {
        async fn start(statuses: Vec<&'static str>) -> Self {
            Self::start_with_body(statuses, r#"{"code":0}"#).await
        }

        async fn start_with_body(statuses: Vec<&'static str>, body: &'static str) -> Self {
            use tokio::io::AsyncWriteExt;
            use tokio::net::TcpListener;

            assert!(!statuses.is_empty());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let server_requests = Arc::clone(&requests);
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let request = read_http_request(&mut socket).await;
                    let (_, request_body) = request.split_once("\r\n\r\n").unwrap();
                    let status = {
                        let mut requests = server_requests.lock().await;
                        let status = statuses[requests.len().min(statuses.len() - 1)];
                        requests.push(serde_json::from_str(request_body).unwrap());
                        status
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
            });
            Self {
                url,
                requests,
                task,
            }
        }

        fn channel(&self, id: &str) -> NotificationChannelConfig {
            NotificationChannelConfig {
                id: id.to_string(),
                kind: NotificationChannelKind::Feishu,
                enabled: true,
                url: self.url.clone(),
                allow_insecure_test_url: true,
                ..NotificationChannelConfig::default()
            }
        }
    }

    impl Drop for MockWebhookServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn isolated_test_state(directory: &Path) -> AppState {
        let mut state = AppState::with_store(ConfigStore::new(directory.join("config.json")));
        state.notification_codex_home = Some(directory.join("codex"));
        state
    }

    fn channel_delivery_test_state(
        directory: &Path,
        channels: Vec<NotificationChannelConfig>,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            store: ConfigStore::new(directory.join("config.json")),
            persisted_waiting_notifications: Mutex::new(WaitingLedgerState::default()),
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels,
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            webhook_http_client_override: Some(
                reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(2))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap(),
            ),
            ..isolated_test_state(directory)
        })
    }

    async fn drive_due(
        state: &Arc<AppState>,
    ) -> Vec<crate::notifications::outbox::DeliveryOutcome> {
        let queue = delivery::outbox(state).await.unwrap();
        let Some(due) = queue.next_due_ms(vec![]).await.unwrap() else {
            return vec![];
        };
        let claimed = queue
            .claim_due(due.max(delivery::now_ms()), 32, vec![])
            .await
            .unwrap();
        let mut results = Vec::new();
        for item in claimed {
            let outcome = delivery::send_current(state, &item).await;
            queue
                .finish(&item, outcome, delivery::now_ms())
                .await
                .unwrap();
            results.push(outcome);
        }
        results
    }

    async fn enqueue_test_event(
        state: &Arc<AppState>,
        channel: NotificationChannelConfig,
        event: &NotificationEvent,
    ) {
        delivery::enqueue(
            state,
            "terminal:session:turn".into(),
            event.clone(),
            vec![channel],
        )
        .await
        .unwrap();
    }

    async fn assert_terminal_delivery_retries_only_unsettled_channels(
        rollout_error: Option<&str>,
        settled_status: &'static str,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let settled_server = MockWebhookServer::start(vec![settled_status]).await;
        let retry_server =
            MockWebhookServer::start(vec!["503 Service Unavailable", "200 OK"]).await;
        let channels = vec![
            settled_server.channel("settled-channel"),
            retry_server.channel("retry-channel"),
        ];
        let state = channel_delivery_test_state(directory.path(), channels.clone());
        let payload = json!({"sessionId":"session-channel-retry","turnId":"turn-channel-retry",
            "sessionName":"Channel retry regression","confirmedByRollout":true,"rolloutError":rollout_error});
        assert_eq!(
            notify_webhook_completion(&state, &payload).await.unwrap()["status"],
            "queued"
        );
        assert!(settled_server.requests.lock().await.is_empty());
        assert_eq!(drive_due(&state).await.len(), 2);
        assert_eq!(
            notify_webhook_completion(&state, &payload).await.unwrap()["status"],
            "duplicate"
        );
        drop(state);
        let restarted = channel_delivery_test_state(directory.path(), channels);
        assert_eq!(drive_due(&restarted).await.len(), 1);
        assert!(drive_due(&restarted).await.is_empty());
        assert_eq!(
            notify_webhook_completion(&restarted, &payload)
                .await
                .unwrap()["status"],
            "duplicate"
        );
        let settled_requests = settled_server.requests.lock().await;
        let retry_requests = retry_server.requests.lock().await;
        assert_eq!(settled_requests.len(), 1);
        assert_eq!(retry_requests.len(), 2);
        let title = if rollout_error.is_some() {
            "Codex会话失败"
        } else {
            "Codex会话完成"
        };
        for request in settled_requests.iter().chain(retry_requests.iter()) {
            assert_eq!(request["card"]["header"]["title"]["content"], title);
        }
    }

    #[tokio::test]
    async fn failed_feishu_notification_is_not_resent_after_an_uncertain_success_response() {
        for body in ["not json", r#"{"msg":"success"}"#, r#"{"code":"0"}"#] {
            let directory = tempfile::tempdir().unwrap();
            let server = MockWebhookServer::start_with_body(vec!["200 OK"], body).await;
            let state =
                channel_delivery_test_state(directory.path(), vec![server.channel("feishu")]);
            let payload = json!({
                "sessionId": "session-ambiguous-response",
                "turnId": "turn-ambiguous-response",
                "sessionName": "Ambiguous response regression",
                "confirmedByRollout": true,
                "rolloutError": "turn failed",
            });

            assert_eq!(
                notify_webhook_completion(&state, &payload).await.unwrap()["status"],
                "queued"
            );
            assert!(matches!(
                drive_due(&state).await.as_slice(),
                [crate::notifications::outbox::DeliveryOutcome::Uncertain]
            ));
            assert!(drive_due(&state).await.is_empty());
            let repeated_scan = notify_webhook_completion(&state, &payload).await.unwrap();
            assert_eq!(repeated_scan["status"], "duplicate");
            assert_eq!(server.requests.lock().await.len(), 1);
        }
    }

    #[tokio::test]
    async fn failed_notification_retries_after_mixed_channel_errors() {
        assert_terminal_delivery_retries_only_unsettled_channels(
            Some("turn failed"),
            "401 Unauthorized",
        )
        .await;
    }

    #[tokio::test]
    async fn completed_notification_retries_after_mixed_channel_errors() {
        assert_terminal_delivery_retries_only_unsettled_channels(None, "401 Unauthorized").await;
    }

    #[tokio::test]
    async fn failed_notification_does_not_resend_a_successful_channel() {
        assert_terminal_delivery_retries_only_unsettled_channels(Some("turn failed"), "200 OK")
            .await;
    }

    #[tokio::test]
    async fn completed_notification_does_not_resend_a_successful_channel() {
        assert_terminal_delivery_retries_only_unsettled_channels(None, "200 OK").await;
    }

    #[tokio::test]
    async fn terminal_notification_settles_when_all_channel_errors_are_permanent() {
        let directory = tempfile::tempdir().unwrap();
        let first = MockWebhookServer::start(vec!["401 Unauthorized"]).await;
        let second = MockWebhookServer::start(vec!["403 Forbidden"]).await;
        let state = channel_delivery_test_state(
            directory.path(),
            vec![first.channel("first"), second.channel("second")],
        );
        let payload = json!({
            "sessionId": "session-permanent",
            "turnId": "turn-permanent",
            "sessionName": "Permanent failures",
            "confirmedByRollout": true,
            "rolloutError": "turn failed",
        });

        notify_webhook_completion(&state, &payload).await.unwrap();
        let outcomes = drive_due(&state).await;
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|outcome| matches!(
            outcome,
            crate::notifications::outbox::DeliveryOutcome::PermanentFailure
        )));
        let duplicate = notify_webhook_completion(&state, &payload).await.unwrap();
        assert_eq!(duplicate["status"], "duplicate");
        assert_eq!(first.requests.lock().await.len(), 1);
        assert_eq!(second.requests.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn mixed_waiting_channel_errors_retry_only_the_failed_channel_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let permanent = MockWebhookServer::start(vec!["401 Unauthorized"]).await;
        let retryable = MockWebhookServer::start(vec!["503 Service Unavailable", "200 OK"]).await;
        let channels = vec![
            permanent.channel("permanent"),
            retryable.channel("retryable"),
        ];
        let state = channel_delivery_test_state(directory.path(), channels.clone());
        let payload = json!({"sessionId":"session-mixed-waiting","turnId":"turn-mixed-waiting",
            "waitingId":"approval-mixed","sessionName":"Mixed waiting failures"});
        assert_eq!(
            notify_webhook_waiting(&state, &payload).await.unwrap()["status"],
            "queued"
        );
        assert_eq!(drive_due(&state).await.len(), 2);
        assert_eq!(
            notify_webhook_waiting(&state, &payload).await.unwrap()["status"],
            "duplicate"
        );
        drop(state);
        let restarted = channel_delivery_test_state(directory.path(), channels);
        assert_eq!(
            notify_webhook_waiting(&restarted, &payload).await.unwrap()["status"],
            "duplicate"
        );
        assert_eq!(drive_due(&restarted).await.len(), 1);
        assert!(drive_due(&restarted).await.is_empty());
        assert_eq!(permanent.requests.lock().await.len(), 1);
        assert_eq!(retryable.requests.lock().await.len(), 2);
    }

    fn lifecycle(started: &[(&str, &str)], completed: &[(&str, &str)]) -> RecentSessionEvents {
        RecentSessionEvents {
            started_turns: Arc::new(
                started
                    .iter()
                    .map(|(session_id, turn_id)| StartedTurn {
                        session_id: (*session_id).to_string(),
                        turn_id: (*turn_id).to_string(),
                    })
                    .collect(),
            ),
            completed_turns: Arc::new(
                completed
                    .iter()
                    .map(|(session_id, turn_id)| CompletedTurn {
                        session_id: (*session_id).to_string(),
                        turn_id: (*turn_id).to_string(),
                        duration_ms: 123,
                        completed_at: None,
                        error: None,
                        is_snapshot_replay: false,
                    })
                    .collect(),
            ),
            ..RecentSessionEvents::default()
        }
    }

    #[tokio::test]
    async fn clawbot_delivery_uses_the_latest_saved_context_token() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut socket).await;
            write_json_response(&mut socket, r#"{"ret":0}"#).await;
            request
        });
        let fresh_channel = NotificationChannelConfig {
            id: "fresh-claw".to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: format!("http://{address}"),
            bot_token: "bot-token".to_string(),
            context_token: "fresh-context".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let mut stale_channel = fresh_channel.clone();
        stale_channel.context_token = "stale-context".to_string();
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels: vec![fresh_channel],
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            ..isolated_test_state(directory.path())
        });
        let event = NotificationEvent::new(
            "session.completed",
            "session-claw",
            "profile-claw",
            "Codex",
            0,
            None,
        );

        enqueue_test_event(&state, stale_channel, &event).await;
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Delivered]
        ));

        let request = server.await.unwrap();
        assert!(request.contains("POST /ilink/bot/sendmessage "));
        assert!(request.contains(r#""context_token":"fresh-context""#));
        assert!(!request.contains("stale-context"));
    }

    #[tokio::test]
    async fn clawbot_stale_delivery_enters_cooldown_and_blocks_followup_requests() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut socket).await;
            write_json_response(&mut socket, r#"{"errcode":-14,"errmsg":"token expired"}"#).await;
            let followup =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
            (request, followup.is_err())
        });
        let channel = NotificationChannelConfig {
            id: "stale-claw".to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: format!("http://{address}"),
            bot_token: "bot-token".to_string(),
            context_token: "context-token".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels: vec![channel.clone()],
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            ..isolated_test_state(directory.path())
        });
        let event = NotificationEvent::new(
            "session.completed",
            "session-claw",
            "profile-claw",
            "Codex",
            0,
            None,
        );

        enqueue_test_event(&state, channel.clone(), &event).await;
        let first = drive_due(&state).await;
        let second = drive_due(&state).await;
        assert!(matches!(
            first.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Retryable { .. }]
        ));
        assert!(matches!(
            second.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Retryable { .. }]
        ));
        assert!(
            crate::commands::wechat_claw_notification_cooldown_remaining(&state, &channel)
                .await
                .is_some()
        );
        let saved = state.config.read().await.webhook.channels[0].clone();
        assert_eq!(saved.bot_token, "bot-token");
        assert_eq!(saved.context_token, "context-token");
        let (request, no_followup_request) = server.await.unwrap();
        assert!(request.starts_with("POST /ilink/bot/sendmessage "));
        assert!(no_followup_request);
    }

    #[tokio::test]
    async fn clawbot_prepare_failure_retries_only_after_context_refresh() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let channel = NotificationChannelConfig {
            id: "refresh-claw".to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: format!("http://{address}"),
            bot_token: "bot-token".to_string(),
            context_token: "stale-context".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels: vec![channel.clone()],
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            ..isolated_test_state(directory.path())
        });
        let server_state = Arc::clone(&state);
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            let (mut first_socket, _) = listener.accept().await.unwrap();
            requests.push(read_http_request(&mut first_socket).await);
            {
                let mut config = server_state.config.write().await;
                config.webhook.channels[0].context_token = "fresh-context".to_string();
                config.webhook.channels[0].context_token_configured = true;
            }
            write_json_response(&mut first_socket, r#"{"ret":-2,"errmsg":"prepare failed"}"#).await;

            let (mut second_socket, _) = listener.accept().await.unwrap();
            requests.push(read_http_request(&mut second_socket).await);
            write_json_response(&mut second_socket, r#"{"ret":0}"#).await;
            requests
        });
        let event = NotificationEvent::new(
            "session.completed",
            "session-claw",
            "profile-claw",
            "Codex",
            0,
            None,
        );

        enqueue_test_event(&state, channel, &event).await;
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Retryable { .. }]
        ));
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Delivered]
        ));

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("POST /ilink/bot/sendmessage "));
        assert!(requests[0].contains(r#""context_token":"stale-context""#));
        assert!(requests[1].starts_with("POST /ilink/bot/sendmessage "));
        assert!(requests[1].contains(r#""context_token":"fresh-context""#));
        assert!(
            requests
                .iter()
                .all(|request| !request.contains("notifystart"))
        );
    }

    #[tokio::test]
    async fn clawbot_prepare_failure_reprepares_session_before_retrying_delivery() {
        use tokio::net::TcpListener;

        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let channel = NotificationChannelConfig {
            id: "self-heal-claw".to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: format!("http://{address}"),
            bot_token: "bot-token".to_string(),
            context_token: "stale-context".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels: vec![channel.clone()],
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            shutting_down: std::sync::atomic::AtomicBool::new(true),
            ..isolated_test_state(directory.path())
        });
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            let responses = [
                r#"{"ret":-2,"errmsg":"prepare failed"}"#.to_string(),
                r#"{"ret":0}"#.to_string(),
                json!({
                    "ret": 0,
                    "get_updates_buf": "fresh-cursor",
                    "msgs": [{
                        "from_user_id": "recipient@im.wechat",
                        "context_token": "stale-context",
                    }]
                })
                .to_string(),
                r#"{"ret":0}"#.to_string(),
            ];
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read_http_request(&mut socket).await);
                write_json_response(&mut socket, &response).await;
            }
            requests
        });
        let event = NotificationEvent::new(
            "session.completed",
            "session-claw",
            "profile-claw",
            "Codex",
            0,
            None,
        );

        enqueue_test_event(&state, channel, &event).await;
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Retryable { .. }]
        ));
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Delivered]
        ));

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].starts_with("POST /ilink/bot/sendmessage "));
        assert!(requests[0].contains(r#""context_token":"stale-context""#));
        assert!(requests[1].starts_with("POST /ilink/bot/msg/notifystart "));
        assert!(requests[2].starts_with("POST /ilink/bot/getupdates "));
        assert!(requests[3].starts_with("POST /ilink/bot/sendmessage "));
        assert!(requests[3].contains(r#""context_token":"stale-context""#));
        let memory = state.config.read().await.clone();
        let disk = state.store.load().unwrap();
        assert_eq!(memory.webhook.channels[0].context_token, "stale-context");
        assert_eq!(disk.webhook.channels[0].get_updates_buf, "fresh-cursor");
    }

    #[test]
    fn webhook_notifications_use_the_matching_turn_configuration() {
        let events = RecentSessionEvents {
            turn_configurations: Arc::new(HashMap::from([(
                "session-1".to_string(),
                HashMap::from([(
                    "turn-1".to_string(),
                    pending_approval::TurnConfiguration {
                        model: "gpt-5.6-luna".to_string(),
                        reasoning_effort: "xhigh".to_string(),
                    },
                )]),
            )])),
            ..RecentSessionEvents::default()
        };

        assert_eq!(
            webhook_turn_configuration(&events, "session-1", "turn-1"),
            ("gpt-5.6-luna".to_string(), "xhigh".to_string())
        );
        assert_eq!(
            webhook_turn_configuration(&events, "session-1", "missing"),
            ("Codex".to_string(), "默认".to_string())
        );
    }

    #[test]
    fn webhook_models_use_the_same_route_prefix_as_the_model_picker() {
        let mut official = crate::config::ProviderProfile::new("官方线路");
        official.id = "official".into();
        official.official_account = true;
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.short_name.clear();
        official.normalize();

        let mut relay = crate::config::ProviderProfile::new("中转线路");
        relay.id = "relay".into();
        relay.short_name = "中转".into();
        relay.base_url = "https://relay.example/v1".into();
        relay.api_key = "relay-key".into();
        relay.normalize();

        let mut config = CodeyConfig {
            profiles: vec![official, relay],
            active_profile_id: "relay".into(),
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        };
        config
            .selected_models_by_provider
            .insert("relay".into(), vec!["claude-opus-4-8".into()]);
        config = config.normalize();

        assert_eq!(
            webhook_display_model(
                &config,
                &local_router::model_alias("relay", "claude-opus-4-8"),
            ),
            "[中转] claude-opus-4-8"
        );
        assert_eq!(
            webhook_display_model(
                &config,
                &local_router::model_alias("official", "gpt-5.6-sol"),
            ),
            "[官方] gpt-5.6-sol"
        );
        assert_eq!(
            webhook_display_model(&config, "claude-opus-4-8"),
            "[中转] claude-opus-4-8"
        );
        assert_eq!(
            webhook_display_model(&config, "unknown-model"),
            "unknown-model"
        );
        assert_eq!(webhook_display_model(&config, ""), "Codex");
    }

    #[test]
    fn idle_session_scans_back_off_gradually_to_thirty_seconds() {
        let events = RecentSessionEvents::default();
        let mut schedule = SessionScanSchedule::default();

        let delays = (0..5)
            .map(|_| schedule.delay_after_scan(&events, &events))
            .collect::<Vec<_>>();

        assert_eq!(
            delays,
            [
                Duration::from_secs(3),
                Duration::from_secs(6),
                Duration::from_secs(12),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
    }

    #[test]
    fn active_or_changed_sessions_keep_the_fast_scan_period() {
        let idle = RecentSessionEvents::default();
        let running = RecentSessionEvents {
            session_statuses: Arc::new(HashMap::from([(
                "session-1".to_string(),
                SessionLifecycleStatus::Running,
            )])),
            ..RecentSessionEvents::default()
        };
        let mut schedule = SessionScanSchedule::default();
        for _ in 0..4 {
            schedule.delay_after_scan(&idle, &idle);
        }

        assert_eq!(
            schedule.delay_after_scan(&idle, &running),
            Duration::from_secs(3)
        );
        assert_eq!(
            schedule.delay_after_scan(&running, &running),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn waking_an_idle_session_scanner_restores_the_fast_period() {
        let events = RecentSessionEvents::default();
        let mut schedule = SessionScanSchedule::default();
        for _ in 0..4 {
            schedule.delay_after_scan(&events, &events);
        }

        schedule.wake();

        assert_eq!(
            schedule.delay_after_scan(&events, &events),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn changing_wait_duration_is_not_treated_as_a_state_change() {
        let waiting_at_first_scan = RecentSessionEvents {
            pending_approvals: vec![PendingApproval {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                waiting_id: "approval-1".to_string(),
                duration_ms: 1_000,
            }],
            ..RecentSessionEvents::default()
        };
        let waiting_later = RecentSessionEvents {
            pending_approvals: vec![PendingApproval {
                duration_ms: 30_000,
                ..waiting_at_first_scan.pending_approvals[0].clone()
            }],
            ..RecentSessionEvents::default()
        };
        assert!(!session_event_state_changed(
            &waiting_at_first_scan,
            &waiting_later
        ));
        assert!(session_events_are_active(&waiting_later));
    }

    #[test]
    fn waiting_notification_ledger_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("webhook-notifications.json");
        let expected = HashSet::from([
            "waiting:session-1:turn-1:approval-1".to_string(),
            "waiting:session-2:turn-2:approval-2".to_string(),
        ]);

        save_waiting_notification_ledger(&path, expected.iter().cloned().collect()).unwrap();

        assert_eq!(load_waiting_notification_ledger(&path), expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn waiting_notification_is_durable_before_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let server = MockWebhookServer::start(vec!["200 OK"]).await;
        let channels = vec![server.channel("waiting")];
        let state = channel_delivery_test_state(directory.path(), channels.clone());
        let payload = json!({"sessionId":"session-durable","turnId":"turn-durable",
            "waitingId":"approval-durable","sessionName":"Durable waiting"});
        assert_eq!(
            notify_webhook_waiting(&state, &payload).await.unwrap()["status"],
            "queued"
        );
        assert!(server.requests.lock().await.is_empty());
        drop(state);
        let restarted = channel_delivery_test_state(directory.path(), channels);
        assert_eq!(
            notify_webhook_waiting(&restarted, &payload).await.unwrap()["status"],
            "duplicate"
        );
        assert!(matches!(
            drive_due(&restarted).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Delivered]
        ));
        assert_eq!(server.requests.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn failed_waiting_delivery_remains_durable_for_retry() {
        let directory = tempfile::tempdir().unwrap();
        let server = MockWebhookServer::start(vec!["503 Service Unavailable", "200 OK"]).await;
        let state = channel_delivery_test_state(directory.path(), vec![server.channel("waiting")]);
        let payload = json!({"sessionId":"session-retry","turnId":"turn-retry",
            "waitingId":"approval-retry","sessionName":"Retry waiting"});
        notify_webhook_waiting(&state, &payload).await.unwrap();
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Retryable { .. }]
        ));
        assert_eq!(
            notify_webhook_waiting(&state, &payload).await.unwrap()["status"],
            "duplicate"
        );
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Delivered]
        ));
        assert!(drive_due(&state).await.is_empty());
    }

    #[tokio::test]
    async fn uncertain_waiting_delivery_is_not_retried_after_restart() {
        use tokio::net::TcpListener;
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_http_request(&mut socket).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        });
        let channels = vec![NotificationChannelConfig {
            id: "timeout".into(),
            url: format!("http://{address}"),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        }];
        let mut state = channel_delivery_test_state(directory.path(), channels.clone());
        Arc::get_mut(&mut state)
            .unwrap()
            .webhook_http_client_override = Some(
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_millis(100))
                .build()
                .unwrap(),
        );
        let payload = json!({"sessionId":"session-timeout","turnId":"turn-timeout",
            "waitingId":"approval-timeout","sessionName":"Timeout waiting"});
        notify_webhook_waiting(&state, &payload).await.unwrap();
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Uncertain]
        ));
        drop(state);
        let restarted = channel_delivery_test_state(directory.path(), channels);
        assert_eq!(
            notify_webhook_waiting(&restarted, &payload).await.unwrap()["status"],
            "duplicate"
        );
        assert!(drive_due(&restarted).await.is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn legacy_waiting_keys_are_tombstones_without_replay() {
        let directory = tempfile::tempdir().unwrap();
        let server = MockWebhookServer::start(vec!["200 OK"]).await;
        let state = channel_delivery_test_state(directory.path(), vec![server.channel("waiting")]);
        let key = waiting_notification_key("legacy", "turn", "approval");
        let path = waiting_notification_ledger_path(&state.store);
        save_waiting_notification_ledger(&path, vec![key]).unwrap();
        *state.persisted_waiting_notifications.lock().await =
            initialize_waiting_notifications(&path, HashSet::new());
        let payload = json!({"sessionId":"legacy","turnId":"turn","waitingId":"approval"});
        assert_eq!(
            notify_webhook_waiting(&state, &payload).await.unwrap()["status"],
            "duplicate"
        );
        assert!(drive_due(&state).await.is_empty());
        assert!(server.requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn changed_channel_binding_cancels_queued_delivery() {
        let directory = tempfile::tempdir().unwrap();
        let original = MockWebhookServer::start(vec!["200 OK"]).await;
        let replacement = MockWebhookServer::start(vec!["200 OK"]).await;
        let state =
            channel_delivery_test_state(directory.path(), vec![original.channel("channel")]);
        let payload = json!({"sessionId":"session","turnId":"turn","sessionName":"Binding change","confirmedByRollout":true});
        notify_webhook_completion(&state, &payload).await.unwrap();
        state.config.write().await.webhook.channels[0] = replacement.channel("channel");
        assert!(matches!(
            drive_due(&state).await.as_slice(),
            [crate::notifications::outbox::DeliveryOutcome::Cancelled]
        ));
        assert!(original.requests.lock().await.is_empty());
        assert!(replacement.requests.lock().await.is_empty());
    }

    #[test]
    fn invalid_waiting_notification_ledger_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("webhook-notifications.json");
        fs::write(&path, b"not-json").unwrap();

        assert!(load_waiting_notification_ledger(&path).is_empty());
    }

    #[test]
    fn existing_waits_become_the_first_run_baseline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("webhook-notifications.json");
        let baseline = HashSet::from(["waiting:session-old:turn-old:approval-old".to_string()]);

        let initialized = initialize_waiting_notifications(&path, baseline.clone());
        assert_eq!(
            initialized.iter().cloned().collect::<HashSet<_>>(),
            baseline
        );
        assert_eq!(load_waiting_notification_ledger(&path), baseline);
    }

    #[test]
    fn waiting_ledger_evicts_oldest_keys_beyond_the_cap() {
        let mut ledger = WaitingLedgerState::default();
        for index in 0..=WEBHOOK_NOTIFICATION_HISTORY_LIMIT {
            ledger.insert(format!("waiting:session:{index}:approval"));
        }

        assert_eq!(
            ledger.ordered_keys().len(),
            WEBHOOK_NOTIFICATION_HISTORY_LIMIT
        );
        assert!(!ledger.contains("waiting:session:0:approval"));
        assert!(ledger.contains(&format!(
            "waiting:session:{WEBHOOK_NOTIFICATION_HISTORY_LIMIT}:approval"
        )));
    }

    #[test]
    fn every_startup_merges_all_existing_waits_into_the_baseline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("webhook-notifications.json");
        let persisted = HashSet::from(["waiting:session-sent:turn-sent:approval-sent".to_string()]);
        save_waiting_notification_ledger(&path, persisted.iter().cloned().collect()).unwrap();
        let existing_wait = "waiting:session-old:turn-old:approval-old".to_string();

        let loaded =
            initialize_waiting_notifications(&path, HashSet::from([existing_wait.clone()]));

        let expected = persisted
            .into_iter()
            .chain([existing_wait])
            .collect::<HashSet<_>>();
        assert_eq!(loaded.iter().cloned().collect::<HashSet<_>>(), expected);
        assert_eq!(load_waiting_notification_ledger(&path), expected);
    }

    #[tokio::test]
    async fn startup_baseline_only_suppresses_waits_seen_before_runtime_start() {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            persisted_waiting_notifications: Mutex::new(WaitingLedgerState::default()),
            ..isolated_test_state(directory.path())
        });
        let before_start = PendingApproval {
            session_id: "session-old".to_string(),
            turn_id: "turn-old".to_string(),
            waiting_id: "approval-old".to_string(),
            duration_ms: 1_000,
        };
        let during_start = PendingApproval {
            session_id: "session-new".to_string(),
            turn_id: "turn-new".to_string(),
            waiting_id: "approval-new".to_string(),
            duration_ms: 100,
        };

        baseline_waiting_notifications(&state, std::slice::from_ref(&before_start)).await;

        let baseline = state.persisted_waiting_notifications.lock().await;
        assert!(baseline.contains(&waiting_notification_key(
            &before_start.session_id,
            &before_start.turn_id,
            &before_start.waiting_id,
        )));
        assert!(!baseline.contains(&waiting_notification_key(
            &during_start.session_id,
            &during_start.turn_id,
            &during_start.waiting_id,
        )));
    }

    #[tokio::test]
    async fn watcher_registration_does_not_wait_for_initial_session_scan() {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(isolated_test_state(directory.path()));
        *state.recent_session_event_cache.lock().await = None;
        let (release_scan_tx, release_scan_rx) = oneshot::channel();
        let initial_scan_task = RecentSessionScanTask::spawn(move || {
            let _ = release_scan_rx.blocking_recv();
            (
                pending_approval::RecentSessionEventCache::default(),
                Arc::new(RecentSessionEvents::default()),
            )
        });

        tokio::time::timeout(
            Duration::from_secs(1),
            start_waiting_webhook_watcher(&state, initial_scan_task),
        )
        .await
        .expect("watcher registration should not await the initial session scan");
        assert!(state.waiting_watcher_task.lock().await.is_some());

        let stop_state = Arc::clone(&state);
        let stop_task =
            tokio::spawn(async move { stop_waiting_webhook_watcher(&stop_state).await });
        tokio::task::yield_now().await;
        assert!(!stop_task.is_finished());
        release_scan_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), stop_task)
            .await
            .expect("watcher stop should finish after the initial scan")
            .unwrap();

        assert!(state.recent_session_event_cache.lock().await.is_some());
    }

    #[tokio::test]
    async fn stopping_waiting_watcher_waits_for_task_and_cache_handoff() {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(isolated_test_state(directory.path()));
        *state.recent_session_event_cache.lock().await = None;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (shutdown_seen_tx, shutdown_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task_completed = Arc::new(AtomicBool::new(false));
        let watcher_state = Arc::clone(&state);
        let watcher_completed = Arc::clone(&task_completed);
        let watcher_task = tokio::spawn(async move {
            let _ = shutdown_rx.await;
            let _ = shutdown_seen_tx.send(());
            let _ = release_rx.await;
            watcher_completed.store(true, Ordering::Release);
            *watcher_state.recent_session_event_cache.lock().await =
                Some(pending_approval::RecentSessionEventCache::default());
        });
        *state.waiting_watcher_shutdown.lock().await = Some(shutdown_tx);
        *state.waiting_watcher_task.lock().await = Some(watcher_task);

        let stop_state = Arc::clone(&state);
        let stop_task =
            tokio::spawn(async move { stop_waiting_webhook_watcher(&stop_state).await });
        shutdown_seen_rx.await.unwrap();
        assert!(!stop_task.is_finished());
        release_tx.send(()).unwrap();
        stop_task.await.unwrap();

        assert!(task_completed.load(Ordering::Acquire));
        assert!(state.waiting_watcher_shutdown.lock().await.is_none());
        assert!(state.waiting_watcher_task.lock().await.is_none());
        assert!(state.recent_session_event_cache.lock().await.is_some());
    }

    #[tokio::test]
    async fn terminal_event_key_is_shared_by_success_and_failure() {
        let directory = tempfile::tempdir().unwrap();
        let server = MockWebhookServer::start(vec!["200 OK"]).await;
        let state = channel_delivery_test_state(directory.path(), vec![server.channel("terminal")]);
        let mut payload = json!({"sessionId":"session","turnId":"turn","sessionName":"Terminal", "confirmedByRollout":true,"rolloutError":"failed"});
        assert_eq!(
            notify_webhook_completion(&state, &payload).await.unwrap()["status"],
            "queued"
        );
        payload["rolloutError"] = Value::Null;
        assert_eq!(
            notify_webhook_completion(&state, &payload).await.unwrap()["status"],
            "duplicate"
        );
        drive_due(&state).await;
        assert_eq!(server.requests.lock().await.len(), 1);
    }

    #[test]
    fn startup_gap_uses_the_initial_scan_timestamp_and_only_baselines_its_snapshot() {
        let baseline = lifecycle(&[("running", "turn")], &[("old", "turn")]);
        let mut tracker = WebhookTurnTracker::from_snapshot_at(&baseline, 100);
        let mut after_registration = lifecycle(
            &[("running", "turn"), ("new", "turn")],
            &[("old", "turn"), ("running", "turn"), ("new", "turn")],
        );
        Arc::make_mut(&mut after_registration.completed_turns)
            .iter_mut()
            .for_each(|turn| turn.completed_at = Some(101));
        let candidates = tracker.completion_candidates(&after_registration);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|turn| turn.session_id != "old"));
    }

    #[test]
    fn webhook_turn_tracker_ignores_completed_snapshot_history() {
        let snapshot = lifecycle(
            &[("session-old", "turn-old")],
            &[("session-old", "turn-old")],
        );
        let mut tracker = WebhookTurnTracker::from_snapshot(&snapshot);

        assert!(tracker.completion_candidates(&snapshot).is_empty());
    }

    #[test]
    fn webhook_turn_tracker_completes_an_initially_running_turn() {
        let snapshot = lifecycle(&[("session-1", "turn-1")], &[]);
        let mut tracker = WebhookTurnTracker::from_snapshot(&snapshot);
        let completed = lifecycle(&[], &[("session-1", "turn-1")]);

        let candidates = tracker.completion_candidates(&completed);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].turn_id, "turn-1");
    }

    #[test]
    fn webhook_turn_tracker_requires_the_same_running_turn() {
        let mut tracker = WebhookTurnTracker::default();
        let events = lifecycle(&[("session-1", "turn-new")], &[("session-1", "turn-old")]);

        assert!(tracker.completion_candidates(&events).is_empty());
    }

    #[test]
    fn webhook_turn_tracker_retries_until_delivery_is_marked_settled() {
        let mut tracker = WebhookTurnTracker::default();
        let events = lifecycle(&[("local:session-1", "turn-1")], &[("session-1", "turn-1")]);

        let first = tracker.completion_candidates(&events);
        assert_eq!(first.len(), 1);
        assert_eq!(tracker.completion_candidates(&events).len(), 1);

        tracker.mark_settled(&first[0]);
        assert!(tracker.completion_candidates(&events).is_empty());
    }

    #[test]
    fn webhook_turn_tracker_bounds_terminal_deduplication_history() {
        let mut tracker = WebhookTurnTracker::default();
        for index in 0..=WEBHOOK_TURN_HISTORY_LIMIT {
            tracker.mark_terminal("session-1", &format!("turn-{index}"));
        }

        assert_eq!(tracker.settled.len(), WEBHOOK_TURN_HISTORY_LIMIT);
        assert_eq!(tracker.settled_order.len(), WEBHOOK_TURN_HISTORY_LIMIT);
        assert!(!tracker.settled.contains("session-1:turn-0"));
        assert!(
            tracker
                .settled
                .contains(&format!("session-1:turn-{WEBHOOK_TURN_HISTORY_LIMIT}"))
        );
    }

    #[test]
    fn webhook_turn_tracker_fences_an_aborted_turn_from_late_completion() {
        let mut tracker = WebhookTurnTracker::default();
        let terminal = RecentSessionEvents {
            started_turns: Arc::new(vec![StartedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
            }]),
            aborted_turns: Arc::new(vec![AbortedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                is_snapshot_replay: false,
            }]),
            ..RecentSessionEvents::default()
        };

        assert!(tracker.completion_candidates(&terminal).is_empty());
        assert!(
            tracker
                .completion_candidates(&lifecycle(&[], &[("session-1", "turn-1")]))
                .is_empty()
        );
    }

    #[test]
    fn webhook_turn_tracker_treats_imported_history_as_snapshot_replay() {
        let mut tracker =
            WebhookTurnTracker::from_snapshot_at(&RecentSessionEvents::default(), 200);
        let mut imported = lifecycle(
            &[("session-imported", "turn-old")],
            &[("session-imported", "turn-old")],
        );
        Arc::make_mut(&mut imported.completed_turns)[0].completed_at = Some(100);

        assert!(tracker.completion_candidates(&imported).is_empty());
    }

    #[test]
    fn webhook_turn_tracker_accepts_a_fast_live_turn_seen_in_one_scan() {
        let mut tracker =
            WebhookTurnTracker::from_snapshot_at(&RecentSessionEvents::default(), 200);
        let mut live = lifecycle(
            &[("session-live", "turn-new")],
            &[("session-live", "turn-new")],
        );
        Arc::make_mut(&mut live.completed_turns)[0].completed_at = Some(200);

        assert_eq!(tracker.completion_candidates(&live).len(), 1);
    }

    #[test]
    fn webhook_turn_tracker_never_notifies_an_import_replay() {
        let mut tracker =
            WebhookTurnTracker::from_snapshot_at(&RecentSessionEvents::default(), 200);
        let mut imported = lifecycle(
            &[("session-imported", "turn-new")],
            &[("session-imported", "turn-new")],
        );
        Arc::make_mut(&mut imported.completed_turns)[0].completed_at = Some(300);
        Arc::make_mut(&mut imported.completed_turns)[0].is_snapshot_replay = true;

        assert!(tracker.completion_candidates(&imported).is_empty());
    }

    #[test]
    fn webhook_turn_tracker_ignores_fork_history_but_completes_the_live_branch_turn() {
        let mut tracker =
            WebhookTurnTracker::from_snapshot_at(&RecentSessionEvents::default(), 200);
        let mut forked = lifecycle(
            &[
                ("session-fork", "turn-inherited"),
                ("session-fork", "turn-live"),
            ],
            &[
                ("session-fork", "turn-inherited"),
                ("session-fork", "turn-live"),
            ],
        );
        Arc::make_mut(&mut forked.completed_turns)[0].completed_at = Some(300);
        Arc::make_mut(&mut forked.completed_turns)[0].is_snapshot_replay = true;
        Arc::make_mut(&mut forked.completed_turns)[1].completed_at = Some(301);

        let candidates = tracker.completion_candidates(&forked);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].turn_id, "turn-live");
    }

    #[tokio::test]
    async fn renderer_completion_hint_cannot_send_without_rollout_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(isolated_test_state(directory.path()));
        let result = notify_webhook_completion(
            &state,
            &json!({
                "sessionId": "session-1",
                "turnId": "turn-1",
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["status"], "skipped");
        assert_eq!(result["reason"], "awaiting-authoritative-rollout-terminal");
    }
}
