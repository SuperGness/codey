use std::collections::{HashSet, VecDeque};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};

use super::AppState;
use crate::codex_config::codex_home;
use crate::config::{CodeyConfig, ConfigStore, OFFICIAL_ROUTE_SHORT_NAME};
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
const WEBHOOK_TURN_HISTORY_LIMIT: usize =
    pending_approval::MAX_RECENT_SESSIONS * pending_approval::MAX_CACHED_TERMINAL_TURNS_PER_ROLLOUT;

#[derive(Debug, Default)]
pub(super) struct WebhookNotificationState {
    in_flight: HashSet<String>,
    settled: HashSet<String>,
    settled_order: VecDeque<String>,
}

impl WebhookNotificationState {
    pub(super) fn from_settled(keys: impl IntoIterator<Item = String>) -> Self {
        let mut state = Self::default();
        state.extend_settled(keys);
        state
    }

    fn is_known(&self, key: &str) -> bool {
        self.in_flight.contains(key) || self.settled.contains(key)
    }

    fn was_settled(&self, key: &str) -> bool {
        self.settled.contains(key)
    }

    fn try_reserve(&mut self, key: String, mutually_exclusive_keys: &[&str]) -> bool {
        if self.is_known(&key)
            || mutually_exclusive_keys
                .iter()
                .any(|candidate| self.is_known(candidate))
        {
            return false;
        }
        self.in_flight.insert(key)
    }

    fn abandon(&mut self, key: &str) {
        self.in_flight.remove(key);
    }

    fn settle(&mut self, key: String) {
        self.in_flight.remove(&key);
        self.remember_settled(key);
    }

    fn extend_settled(&mut self, keys: impl IntoIterator<Item = String>) {
        for key in keys {
            self.remember_settled(key);
        }
    }

    fn remember_settled(&mut self, key: String) {
        if self.settled.insert(key.clone()) {
            self.settled_order.push_back(key);
        }
        while self.settled_order.len() > WEBHOOK_NOTIFICATION_HISTORY_LIMIT {
            if let Some(oldest) = self.settled_order.pop_front() {
                self.settled.remove(&oldest);
            }
        }
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

    fn remove(&mut self, key: &str) -> bool {
        if !self.keys.remove(key) {
            return false;
        }
        self.order.retain(|candidate| candidate != key);
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
pub(super) type RecentSessionScanTask = tokio::task::JoinHandle<RecentSessionScanResult>;

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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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

async fn reserve_waiting_notification(
    state: &Arc<AppState>,
    notification_key: &str,
) -> Result<bool, String> {
    {
        let mut persisted = state.persisted_waiting_notifications.lock().await;
        if persisted.contains(notification_key) {
            return Ok(false);
        }
        persisted.insert(notification_key.to_string());
    }
    // The write-ahead contract stays: the reservation must be durable before
    // the caller sends. Failure rolls the key back; a concurrent duplicate
    // that skipped on the in-flight key stays suppressed, which matches the
    // documented at-most-once boundary.
    if let Err(error) = persist_waiting_ledger(state).await {
        state
            .persisted_waiting_notifications
            .lock()
            .await
            .remove(notification_key);
        return Err(error);
    }
    Ok(true)
}

async fn release_waiting_notification(
    state: &Arc<AppState>,
    notification_key: &str,
) -> Result<(), String> {
    {
        let mut persisted = state.persisted_waiting_notifications.lock().await;
        if !persisted.remove(notification_key) {
            return Ok(());
        }
    }
    if let Err(error) = persist_waiting_ledger(state).await {
        state
            .persisted_waiting_notifications
            .lock()
            .await
            .insert(notification_key.to_string());
        return Err(error);
    }
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
    state
        .webhook_notifications
        .lock()
        .await
        .extend_settled(baseline);
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
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let watcher_state = Arc::clone(state);
    let watcher_task = tokio::spawn(async move {
        let (mut event_cache, baseline_events) = await_recent_session_scan(initial_scan_task).await;
        if !matches!(
            shutdown_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ) {
            event_cache.release_database_connections();
            *watcher_state.recent_session_event_cache.lock().await = Some(event_cache);
            return;
        }
        baseline_waiting_notifications(&watcher_state, &baseline_events.pending_approvals).await;
        let mut turn_tracker = WebhookTurnTracker::from_snapshot(&baseline_events);
        let mut previous_events = baseline_events;
        let mut scan_schedule = SessionScanSchedule::default();
        let mut next_scan_delay = ACTIVE_SESSION_SCAN_DELAY;
        let mut scan_immediately = true;
        loop {
            if !scan_immediately {
                let woke = tokio::select! {
                    _ = &mut shutdown_rx => break,
                    _ = watcher_state.session_scan_wake.notified() => true,
                    _ = tokio::time::sleep(next_scan_delay) => false,
                };
                if woke {
                    scan_schedule.wake();
                }
            }
            scan_immediately = false;

            let (next_cache, events) = scan_recent_session_events(event_cache).await;
            event_cache = next_cache;
            notify_pending_approvals(&watcher_state, &events).await;
            let mut completion_delivery_pending = false;
            for completed in turn_tracker.completion_candidates(&events) {
                let (model, reasoning_effort) =
                    webhook_turn_configuration(&events, &completed.session_id, &completed.turn_id);
                let payload = json!({
                    "sessionId": completed.session_id,
                    "turnId": completed.turn_id,
                    "durationMs": completed.duration_ms,
                    "rolloutError": completed.error,
                    "confirmedByRollout": true,
                    "model": model,
                    "reasoningEffort": reasoning_effort,
                });
                match notify_webhook_completion(&watcher_state, &payload).await {
                    Ok(_) => turn_tracker.mark_settled(&completed),
                    Err(error) => {
                        // Keep the running edge so a transient delivery failure is retried.
                        completion_delivery_pending = true;
                        eprintln!("Codey 完成通知失败：{error}");
                    }
                }
            }
            next_scan_delay = if completion_delivery_pending {
                scan_schedule.wake();
                ACTIVE_SESSION_SCAN_DELAY
            } else {
                scan_schedule.delay_after_scan(&previous_events, &events)
            };
            previous_events = events;
        }
        event_cache.release_database_connections();
        *watcher_state.recent_session_event_cache.lock().await = Some(event_cache);
    });
    *state.waiting_watcher_shutdown.lock().await = Some(shutdown_tx);
    *state.waiting_watcher_task.lock().await = Some(watcher_task);
}

async fn notify_pending_approvals(state: &Arc<AppState>, events: &RecentSessionEvents) {
    for pending in &events.pending_approvals {
        let (model, reasoning_effort) =
            webhook_turn_configuration(events, &pending.session_id, &pending.turn_id);
        let payload = json!({
            "sessionId": pending.session_id,
            "turnId": pending.turn_id,
            "waitingId": pending.waiting_id,
            "durationMs": pending.duration_ms,
            "model": model,
            "reasoningEffort": reasoning_effort,
        });
        if let Err(error) = notify_webhook_waiting(state, &payload).await {
            eprintln!("Codey 等待通知失败：{error}");
        }
    }
}

async fn scan_recent_session_events(
    cache: pending_approval::RecentSessionEventCache,
) -> RecentSessionScanResult {
    await_recent_session_scan(start_recent_session_scan(cache)).await
}

pub(super) fn start_recent_session_scan(
    mut cache: pending_approval::RecentSessionEventCache,
) -> RecentSessionScanTask {
    let home = codex_home();
    tokio::task::spawn_blocking(move || {
        let events = cache.refresh(home);
        (cache, events)
    })
}

pub(super) async fn await_recent_session_scan(
    scan_task: RecentSessionScanTask,
) -> RecentSessionScanResult {
    match scan_task.await {
        Ok(result) => result,
        Err(error) => {
            eprintln!("Codey 会话状态扫描任务异常退出：{error}");
            (
                pending_approval::RecentSessionEventCache::default(),
                Arc::new(RecentSessionEvents::default()),
            )
        }
    }
}

pub(super) async fn stop_waiting_webhook_watcher(state: &Arc<AppState>) {
    let shutdown = state.waiting_watcher_shutdown.lock().await.take();
    if let Some(shutdown) = shutdown {
        let _ = shutdown.send(());
    }
    let watcher_task = state.waiting_watcher_task.lock().await.take();
    if let Some(watcher_task) = watcher_task
        && let Err(error) = watcher_task.await
    {
        eprintln!("Codey 会话状态 watcher 异常退出：{error}");
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

    // reqwest snapshots the operating system proxy configuration when a client
    // is built. Notifications are infrequent, so create one client per test or
    // delivery batch instead of retaining stale proxy settings for the entire
    // Codey process lifetime.
    crate::notifications::notification_http_client().map_err(|error| error.to_string())
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
    let prefix = if profile.official_account {
        OFFICIAL_ROUTE_SHORT_NAME
    } else {
        profile.short_name.trim()
    };
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
async fn webhook_session_name(state: &Arc<AppState>, payload: &Value, session_id: &str) -> String {
    let requested_title = payload
        .get("sessionName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty());
    if let Some(title) = requested_title {
        state
            .session_titles
            .write()
            .await
            .insert(session_id.to_string(), title.to_string());
    }
    let cached_title = state.session_titles.read().await.get(session_id).cloned();
    let fallback_title = cached_title.clone();
    let home = codex_home();
    let session_id = session_id.to_string();
    match super::resolve_session_name_cached(state, home.to_path_buf(), session_id, cached_title)
        .await
    {
        Ok(session_name) => session_name,
        Err(error) => {
            eprintln!("读取通知会话名称任务异常退出：{error}");
            fallback_title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| "未命名会话".to_string())
        }
    }
}

fn terminal_notification_keys(session_id: &str, turn_id: &str) -> [String; 2] {
    [
        format!("completed:{session_id}:{turn_id}"),
        format!("failed:{session_id}:{turn_id}"),
    ]
}

async fn terminal_notification_was_sent(
    state: &Arc<AppState>,
    session_id: &str,
    turn_id: &str,
) -> bool {
    let keys = terminal_notification_keys(session_id, turn_id);
    let notifications = state.webhook_notifications.lock().await;
    keys.iter().any(|key| notifications.is_known(key))
}

#[derive(Debug)]
struct WebhookDispatchError {
    detail: String,
    settle_delivery: bool,
}

impl WebhookDispatchError {
    fn should_settle_delivery(&self) -> bool {
        self.settle_delivery
    }
}

impl std::fmt::Display for WebhookDispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

async fn dispatch_webhook_channels(
    state: &Arc<AppState>,
    channels: Vec<NotificationChannelConfig>,
    event: &NotificationEvent,
    notification_key: &str,
) -> Result<(), WebhookDispatchError> {
    let mut deliveries = channels
        .into_iter()
        .filter(|channel| channel.enabled && channel.is_configured())
        .map(|channel| {
            let delivery_key = format!("channel:{}:{notification_key}", channel.id);
            (channel, delivery_key)
        })
        .collect::<Vec<_>>();
    {
        let notifications = state.webhook_notifications.lock().await;
        deliveries.retain(|(_, delivery_key)| !notifications.was_settled(delivery_key));
    }
    if deliveries.is_empty() {
        return Ok(());
    }
    let client =
        current_notification_http_client(state).map_err(|detail| WebhookDispatchError {
            detail: format!("创建通知网络客户端失败：{detail}"),
            settle_delivery: false,
        })?;
    let deliveries = deliveries.into_iter().map(|(channel, delivery_key)| {
        let client = client.clone();
        let event = event.clone();
        let state = Arc::clone(state);
        async move {
            (
                delivery_key,
                dispatch_webhook_channel(&state, client, channel, &event).await,
            )
        }
    });
    let results = collect_webhook_deliveries(deliveries).await;
    let mut errors = Vec::new();
    let mut settle_delivery = false;
    let mut notifications = state.webhook_notifications.lock().await;
    for (delivery_key, result) in results {
        match result {
            Ok(()) => {
                notifications.remember_settled(delivery_key);
            }
            Err(error) => {
                if error.should_settle_delivery() {
                    settle_delivery = true;
                    notifications.remember_settled(delivery_key);
                }
                errors.push(error.to_string());
            }
        }
    }
    drop(notifications);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(WebhookDispatchError {
            detail: format!("部分通知渠道发送失败：{}", errors.join("；")),
            settle_delivery,
        })
    }
}

async fn dispatch_webhook_channel(
    state: &Arc<AppState>,
    client: reqwest::Client,
    channel: NotificationChannelConfig,
    event: &NotificationEvent,
) -> Result<(), WebhookDispatchError> {
    let channel = latest_channel_for_delivery(state, &channel).await;
    if channel.kind == NotificationChannelKind::WechatClaw
        && let Some(remaining) =
            super::wechat_claw_notification_cooldown_remaining(state, &channel).await
    {
        return Err(WebhookDispatchError {
            detail: wechat_claw_cooldown_message(remaining),
            settle_delivery: false,
        });
    }
    let dispatcher = NotificationDispatcher::with_client(client.clone(), channel.clone());
    match dispatcher.send(event).await {
        Ok(()) => Ok(()),
        Err(error)
            if channel.kind == NotificationChannelKind::WechatClaw
                && error.indicates_stale_token() =>
        {
            let remaining = super::pause_wechat_claw_notification_channel(state, &channel)
                .await
                .unwrap_or_else(|| Duration::from_secs(60 * 60));
            Err(WebhookDispatchError {
                detail: format!("{error}（{}）", wechat_claw_cooldown_message(remaining)),
                settle_delivery: false,
            })
        }
        Err(error)
            if channel.kind == NotificationChannelKind::WechatClaw
                && error.should_retry_with_fresh_context() =>
        {
            let mut fresh_channel = latest_channel_for_delivery(state, &channel).await;
            if !fresh_channel.enabled || !fresh_channel.is_configured() {
                return Ok(());
            }
            if fresh_channel.context_token.trim() == channel.context_token.trim() {
                let refresh_result =
                    super::refresh_wechat_claw_channel_context(state, &channel).await;
                fresh_channel = latest_channel_for_delivery(state, &channel).await;
                if !fresh_channel.enabled || !fresh_channel.is_configured() {
                    if let Err(refresh_error) = refresh_result {
                        return Err(WebhookDispatchError {
                            detail: format!("{error}（ClawBot 同步刷新失败：{refresh_error}）"),
                            settle_delivery: false,
                        });
                    }
                    return Ok(());
                }
                if let Err(refresh_error) = refresh_result {
                    return Err(WebhookDispatchError {
                        detail: format!("{error}（ClawBot 同步重建失败：{refresh_error}）"),
                        settle_delivery: false,
                    });
                }
            }
            let dispatcher = NotificationDispatcher::with_client(client, fresh_channel);
            match dispatcher.send(event).await {
                Ok(()) => Ok(()),
                Err(error) => Err(WebhookDispatchError {
                    detail: error.to_string(),
                    settle_delivery: error.should_settle_delivery(),
                }),
            }
        }
        Err(error) => Err(WebhookDispatchError {
            detail: error.to_string(),
            settle_delivery: error.should_settle_delivery(),
        }),
    }
}

fn wechat_claw_cooldown_message(remaining: Duration) -> String {
    let minutes = (remaining.as_secs().saturating_add(59) / 60).max(1);
    format!("微信 ClawBot 凭据暂时不可用，约 {minutes} 分钟后自动重试")
}

async fn latest_channel_for_delivery(
    state: &Arc<AppState>,
    channel: &NotificationChannelConfig,
) -> NotificationChannelConfig {
    if channel.kind != NotificationChannelKind::WechatClaw {
        return channel.clone();
    }
    let mut delivery = channel.clone();
    let config = state.config.read().await;
    let Some(saved) =
        config.webhook.channels.iter().find(|saved| {
            saved.id == channel.id && saved.kind == NotificationChannelKind::WechatClaw
        })
    else {
        delivery.enabled = false;
        return delivery;
    };
    let same_binding = saved.enabled
        && saved.bot_token.trim() == channel.bot_token.trim()
        && saved.chat_id.trim() == channel.chat_id.trim()
        && saved.wechat_claw_base_url().ok() == channel.wechat_claw_base_url().ok();
    if !same_binding {
        delivery.enabled = false;
        return delivery;
    }
    delivery.context_token = saved.context_token.clone();
    delivery.context_token_configured = saved.context_token_configured;
    delivery.get_updates_buf = saved.get_updates_buf.clone();
    delivery
}

async fn collect_webhook_deliveries<F, T>(deliveries: impl IntoIterator<Item = F>) -> Vec<T>
where
    F: Future<Output = T>,
{
    let mut results: Vec<(usize, T)> = stream::iter(
        deliveries
            .into_iter()
            .enumerate()
            .map(|(index, delivery)| async move { (index, delivery.await) }),
    )
    .buffer_unordered(MAX_CONCURRENT_WEBHOOK_DELIVERIES)
    .collect()
    .await;
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_settled_webhook_failure(
    state: &Arc<AppState>,
    payload: &Value,
    session_id: String,
    turn_id: String,
    profile_id: String,
    model: String,
    reasoning_effort: String,
    duration_ms: u128,
    error: String,
) -> Result<Value, String> {
    let channels = state.config.read().await.webhook.channels.clone();
    if !channels
        .iter()
        .any(|channel| channel.enabled && channel.is_configured())
    {
        return Ok(json!({"status":"skipped","reason":"disabled"}));
    }

    let [completed_key, failed_key] = terminal_notification_keys(&session_id, &turn_id);
    {
        let mut notifications = state.webhook_notifications.lock().await;
        if !notifications.try_reserve(failed_key.clone(), &[&completed_key]) {
            return Ok(json!({"status":"duplicate"}));
        }
    }
    let session_name = webhook_session_name(state, payload, &session_id).await;
    let event = NotificationEvent::new(
        "session.failed",
        session_id,
        profile_id,
        model,
        duration_ms,
        Some(error),
    )
    .with_session_name(session_name)
    .with_reasoning_effort(reasoning_effort);
    if let Err(error) = dispatch_webhook_channels(state, channels, &event, &failed_key).await {
        let mut notifications = state.webhook_notifications.lock().await;
        if error.should_settle_delivery() {
            notifications.settle(failed_key);
        } else {
            notifications.abandon(&failed_key);
        }
        return Err(error.to_string());
    }
    state.webhook_notifications.lock().await.settle(failed_key);
    Ok(json!({"status":"ok","eventId":event.event_id}))
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
    if terminal_notification_was_sent(state, &session_id, &turn_id).await {
        return Ok(json!({"status":"duplicate"}));
    }
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
    if let Some(error) = rollout_error {
        return dispatch_settled_webhook_failure(
            state,
            payload,
            session_id,
            turn_id,
            profile_id,
            model,
            reasoning_effort,
            requested_duration_ms,
            error,
        )
        .await;
    }

    let notification_key = format!("completed:{session_id}:{turn_id}");
    {
        let mut notifications = state.webhook_notifications.lock().await;
        let failed_key = format!("failed:{session_id}:{turn_id}");
        if !notifications.try_reserve(notification_key.clone(), &[&failed_key]) {
            return Ok(json!({"status":"duplicate"}));
        }
    }
    let session_name = webhook_session_name(state, payload, &session_id).await;
    let event = NotificationEvent::new(
        "session.completed",
        session_id,
        profile_id,
        model,
        requested_duration_ms,
        None,
    )
    .with_session_name(session_name)
    .with_reasoning_effort(reasoning_effort);
    if let Err(error) = dispatch_webhook_channels(state, channels, &event, &notification_key).await
    {
        let mut notifications = state.webhook_notifications.lock().await;
        if error.should_settle_delivery() {
            notifications.settle(notification_key);
        } else {
            notifications.abandon(&notification_key);
        }
        return Err(error.to_string());
    }
    state
        .webhook_notifications
        .lock()
        .await
        .settle(notification_key);
    Ok(json!({"status":"ok","eventId":event.event_id}))
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
    {
        let mut notifications = state.webhook_notifications.lock().await;
        if !notifications.try_reserve(notification_key.clone(), &[]) {
            return Ok(json!({"status":"duplicate"}));
        }
    }
    // Feishu and Telegram webhooks provide no usable idempotency key. Reserve
    // on disk before sending so a crash after remote acceptance cannot resend
    // the same waiting notification after restart.
    match reserve_waiting_notification(state, &notification_key).await {
        Ok(true) => {}
        Ok(false) => {
            state
                .webhook_notifications
                .lock()
                .await
                .abandon(&notification_key);
            return Ok(json!({"status":"duplicate"}));
        }
        Err(error) => {
            state
                .webhook_notifications
                .lock()
                .await
                .abandon(&notification_key);
            return Err(error);
        }
    }
    let session_name = webhook_session_name(state, payload, &session_id).await;
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
    if let Err(error) = dispatch_webhook_channels(state, channels, &event, &notification_key).await
    {
        if error.should_settle_delivery() {
            state
                .webhook_notifications
                .lock()
                .await
                .settle(notification_key);
            return Err(error.to_string());
        }
        let rollback_error = release_waiting_notification(state, &notification_key)
            .await
            .err();
        state
            .webhook_notifications
            .lock()
            .await
            .abandon(&notification_key);
        if let Some(rollback_error) = rollback_error {
            return Err(format!("{error}；等待通知预留回滚失败：{rollback_error}"));
        }
        return Err(error.to_string());
    }
    state
        .webhook_notifications
        .lock()
        .await
        .settle(notification_key.clone());
    Ok(json!({"status":"ok","eventId":event.event_id}))
}
#[cfg(test)]
mod tests {
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
    async fn webhook_delivery_concurrency_is_bounded() {
        let delivery_count = MAX_CONCURRENT_WEBHOOK_DELIVERIES + 3;
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Semaphore::new(0));
        let deliveries = (0..delivery_count)
            .map(|_| {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                let gate = Arc::clone(&gate);
                async move {
                    let current = active.fetch_add(1, Ordering::AcqRel) + 1;
                    peak.fetch_max(current, Ordering::AcqRel);
                    let permit = gate.acquire_owned().await.unwrap();
                    permit.forget();
                    active.fetch_sub(1, Ordering::AcqRel);
                }
            })
            .collect::<Vec<_>>();
        let delivery_task = tokio::spawn(collect_webhook_deliveries(deliveries));

        tokio::time::timeout(Duration::from_secs(1), async {
            while peak.load(Ordering::Acquire) < MAX_CONCURRENT_WEBHOOK_DELIVERIES {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the bounded delivery window should fill");
        assert_eq!(
            peak.load(Ordering::Acquire),
            MAX_CONCURRENT_WEBHOOK_DELIVERIES
        );

        gate.add_permits(delivery_count);
        delivery_task.await.unwrap();
        assert_eq!(active.load(Ordering::Acquire), 0);
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
        let state = Arc::new(AppState {
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels: vec![fresh_channel],
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            ..AppState::default()
        });
        let event = NotificationEvent::new(
            "session.completed",
            "session-claw",
            "profile-claw",
            "Codex",
            0,
            None,
        );

        dispatch_webhook_channels(
            &state,
            vec![stale_channel],
            &event,
            "completed:session:turn",
        )
        .await
        .unwrap();

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
        let state = Arc::new(AppState {
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels: vec![channel.clone()],
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            ..AppState::default()
        });
        let event = NotificationEvent::new(
            "session.completed",
            "session-claw",
            "profile-claw",
            "Codex",
            0,
            None,
        );

        let first = dispatch_webhook_channels(
            &state,
            vec![channel.clone()],
            &event,
            "completed:session:turn",
        )
        .await
        .unwrap_err();
        let second = dispatch_webhook_channels(
            &state,
            vec![channel.clone()],
            &event,
            "completed:session:turn",
        )
        .await
        .unwrap_err();

        assert!(first.to_string().contains("自动重试"));
        assert!(second.to_string().contains("自动重试"));
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
        let state = Arc::new(AppState {
            config: RwLock::new(CodeyConfig {
                webhook: crate::notifications::WebhookConfig {
                    channels: vec![channel.clone()],
                    ..crate::notifications::WebhookConfig::default()
                },
                ..CodeyConfig::default()
            }),
            ..AppState::default()
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

        dispatch_webhook_channels(&state, vec![channel], &event, "completed:session:turn")
            .await
            .unwrap();

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
            ..AppState::default()
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

        dispatch_webhook_channels(&state, vec![channel], &event, "completed:session:turn")
            .await
            .unwrap();

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
            "[官] gpt-5.6-sol"
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
    fn bounded_notification_history_never_evicts_an_in_flight_reservation() {
        let mut notifications = WebhookNotificationState::default();
        let in_flight = "completed:active-session:active-turn".to_string();
        assert!(notifications.try_reserve(in_flight.clone(), &[]));

        for index in 0..=WEBHOOK_NOTIFICATION_HISTORY_LIMIT {
            notifications.remember_settled(format!("completed:old-session:{index}"));
        }

        assert!(notifications.is_known(&in_flight));
        assert!(!notifications.try_reserve(in_flight.clone(), &[]));
        assert_eq!(
            notifications.settled.len(),
            WEBHOOK_NOTIFICATION_HISTORY_LIMIT
        );
        assert!(!notifications.was_settled("completed:old-session:0"));
        assert!(notifications.was_settled(&format!(
            "completed:old-session:{WEBHOOK_NOTIFICATION_HISTORY_LIMIT}"
        )));
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
    async fn waiting_notification_is_reserved_on_disk_before_delivery() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_seen_tx, request_seen_rx) = oneshot::channel();
        let (respond_tx, respond_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let bytes_read = socket.read(&mut request).await.unwrap();
            assert!(bytes_read > 0);
            request_seen_tx.send(()).unwrap();
            respond_rx.await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 10\r\nconnection: close\r\n\r\n{\"code\":0}",
                )
                .await
                .unwrap();
        });
        let mut config = CodeyConfig::default();
        config.webhook.channels = vec![NotificationChannelConfig {
            id: "local-feishu".to_string(),
            kind: NotificationChannelKind::Feishu,
            enabled: true,
            url: format!("http://{address}"),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        }];
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(config),
            webhook_notifications: Mutex::new(WebhookNotificationState::default()),
            persisted_waiting_notifications: Mutex::new(WaitingLedgerState::default()),
            ..AppState::default()
        });
        let notification_key =
            waiting_notification_key("session-durable", "turn-durable", "approval-durable");
        let payload = json!({
            "sessionId": "session-durable",
            "turnId": "turn-durable",
            "waitingId": "approval-durable",
            "sessionName": "Durable waiting notification",
        });
        let notify_state = Arc::clone(&state);
        let notify_task =
            tokio::spawn(async move { notify_webhook_waiting(&notify_state, &payload).await });

        tokio::time::timeout(Duration::from_secs(2), request_seen_rx)
            .await
            .expect("notification request should reach the local server")
            .unwrap();
        assert!(
            load_waiting_notification_ledger(&waiting_notification_ledger_path(&state.store))
                .contains(&notification_key)
        );

        respond_tx.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), notify_task)
            .await
            .expect("notification delivery should finish")
            .unwrap()
            .unwrap();
        assert_eq!(result["status"], "ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn failed_waiting_delivery_releases_the_durable_reservation() {
        use tokio::net::TcpListener;

        let directory = tempfile::tempdir().unwrap();
        let unused_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unused_address = unused_listener.local_addr().unwrap();
        drop(unused_listener);
        let mut config = CodeyConfig::default();
        config.webhook.channels = vec![NotificationChannelConfig {
            id: "unreachable-feishu".to_string(),
            kind: NotificationChannelKind::Feishu,
            enabled: true,
            url: format!("http://{unused_address}"),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        }];
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(config),
            webhook_notifications: Mutex::new(WebhookNotificationState::default()),
            persisted_waiting_notifications: Mutex::new(WaitingLedgerState::default()),
            ..AppState::default()
        });
        let notification_key =
            waiting_notification_key("session-retry", "turn-retry", "approval-retry");
        let payload = json!({
            "sessionId": "session-retry",
            "turnId": "turn-retry",
            "waitingId": "approval-retry",
            "sessionName": "Retry waiting notification",
        });

        assert!(notify_webhook_waiting(&state, &payload).await.is_err());

        assert!(
            !load_waiting_notification_ledger(&waiting_notification_ledger_path(&state.store))
                .contains(&notification_key)
        );
        assert!(
            !state
                .persisted_waiting_notifications
                .lock()
                .await
                .contains(&notification_key)
        );
        assert!(
            !state
                .webhook_notifications
                .lock()
                .await
                .is_known(&notification_key)
        );
    }

    #[tokio::test]
    async fn uncertain_waiting_delivery_keeps_the_durable_reservation() {
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let bytes_read = socket.read(&mut request).await.unwrap();
            assert!(bytes_read > 0);
            server_request_count.fetch_add(1, Ordering::AcqRel);
            tokio::time::sleep(Duration::from_millis(1_500)).await;
        });
        let mut config = CodeyConfig::default();
        config.webhook.channels = vec![NotificationChannelConfig {
            id: "timeout-feishu".to_string(),
            kind: NotificationChannelKind::Feishu,
            enabled: true,
            url: format!("http://{address}"),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        }];
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(config),
            webhook_http_client_override: Some(
                reqwest::Client::builder()
                    .timeout(Duration::from_millis(500))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap(),
            ),
            webhook_notifications: Mutex::new(WebhookNotificationState::default()),
            persisted_waiting_notifications: Mutex::new(WaitingLedgerState::default()),
            ..AppState::default()
        });
        let notification_key =
            waiting_notification_key("session-uncertain", "turn-uncertain", "approval-uncertain");
        let payload = json!({
            "sessionId": "session-uncertain",
            "turnId": "turn-uncertain",
            "waitingId": "approval-uncertain",
            "sessionName": "Uncertain waiting notification",
        });

        let error = notify_webhook_waiting(&state, &payload).await.unwrap_err();
        assert!(error.contains("发送结果不确定"));
        assert!(
            load_waiting_notification_ledger(&waiting_notification_ledger_path(&state.store))
                .contains(&notification_key)
        );
        let duplicate = notify_webhook_waiting(&state, &payload).await.unwrap();
        assert_eq!(duplicate["status"], "duplicate");
        server.await.unwrap();
        assert_eq!(request_count.load(Ordering::Acquire), 1);
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
            webhook_notifications: Mutex::new(WebhookNotificationState::default()),
            persisted_waiting_notifications: Mutex::new(WaitingLedgerState::default()),
            ..AppState::default()
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

        let baseline = state.webhook_notifications.lock().await;
        assert!(baseline.is_known(&waiting_notification_key(
            &before_start.session_id,
            &before_start.turn_id,
            &before_start.waiting_id,
        )));
        assert!(!baseline.is_known(&waiting_notification_key(
            &during_start.session_id,
            &during_start.turn_id,
            &during_start.waiting_id,
        )));
    }

    #[tokio::test]
    async fn watcher_registration_does_not_wait_for_initial_session_scan() {
        let state = Arc::new(AppState::default());
        *state.recent_session_event_cache.lock().await = None;
        let (release_scan_tx, release_scan_rx) = oneshot::channel();
        let initial_scan_task = tokio::spawn(async move {
            let _ = release_scan_rx.await;
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
        let state = Arc::new(AppState::default());
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

    #[test]
    fn terminal_notification_keys_cover_success_and_failure() {
        assert_eq!(
            terminal_notification_keys("session-1", "turn-1"),
            [
                "completed:session-1:turn-1".to_string(),
                "failed:session-1:turn-1".to_string(),
            ]
        );
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
        let state = Arc::new(AppState::default());
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
