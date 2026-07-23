use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codex_plus_core::app_paths::{build_codex_executable, normalize_codex_app_path};
use futures_util::StreamExt;
use reqwest::header::USER_AGENT;
use semver::Version;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify, RwLock, oneshot};

use crate::cc_switch;
use crate::cdp;
use crate::codex_config::codex_home;
use crate::config::{CodeyConfig, ConfigStore};
use crate::launcher::{CodeyRuntime, restore_previous_runtime_state, restore_runtime_config};
use crate::message_delete::delete_messages;
use crate::model_catalog;
use crate::pending_approval;
use crate::pending_approval::{CompletedTurn, RecentSessionEvents, SessionLifecycleStatus};
use crate::plugin_marketplace;
use crate::provider_models;
use crate::session_delete;
use crate::session_metadata;
use crate::session_transfer;
use crate::trace_log_guard;
use crate::trace_log_stats::{self, TraceLogStatsHandle, TraceLogStatsSnapshot};
use crate::webhook::{WebhookDispatcher, WebhookEvent};

pub struct AppState {
    pub store: ConfigStore,
    pub config: RwLock<CodeyConfig>,
    pub http_client: reqwest::Client,
    pub runtime: Mutex<Option<Arc<CodeyRuntime>>>,
    pub trace_log_stats: TraceLogStatsHandle,
    pub startup_error: RwLock<Option<String>>,
    restart_in_progress: AtomicBool,
    runtime_generation: AtomicU64,
    session_titles: RwLock<HashMap<String, String>>,
    webhook_notifications: Mutex<HashSet<String>>,
    persisted_waiting_notifications: Mutex<HashSet<String>>,
    recent_session_event_cache: Mutex<Option<pending_approval::RecentSessionEventCache>>,
    waiting_watcher_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    waiting_watcher_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    session_scan_wake: Notify,
    shutdown_reason: AtomicU8,
    shutdown_notify: Notify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppShutdownReason {
    CodexExited,
    InstallUpdate,
}

impl AppShutdownReason {
    const NONE: u8 = 0;
    const CODEX_EXITED: u8 = 1;
    const INSTALL_UPDATE: u8 = 2;

    fn as_u8(self) -> u8 {
        match self {
            Self::CodexExited => Self::CODEX_EXITED,
            Self::InstallUpdate => Self::INSTALL_UPDATE,
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        match value {
            Self::CODEX_EXITED => Some(Self::CodexExited),
            Self::INSTALL_UPDATE => Some(Self::InstallUpdate),
            _ => None,
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        let store = ConfigStore::default();
        let config = store.load().unwrap_or_default();
        let persisted_waiting_notifications = initial_waiting_notifications(&store, &[]);
        Self {
            store,
            config: RwLock::new(config),
            http_client: reqwest::Client::builder()
                .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("shared Codey HTTP client should be constructible"),
            runtime: Mutex::new(None),
            trace_log_stats: TraceLogStatsHandle::idle(),
            startup_error: RwLock::new(None),
            restart_in_progress: AtomicBool::new(false),
            runtime_generation: AtomicU64::new(0),
            session_titles: RwLock::new(HashMap::new()),
            webhook_notifications: Mutex::new(persisted_waiting_notifications.clone()),
            persisted_waiting_notifications: Mutex::new(persisted_waiting_notifications),
            recent_session_event_cache: Mutex::new(Some(
                pending_approval::RecentSessionEventCache::default(),
            )),
            waiting_watcher_shutdown: Mutex::new(None),
            waiting_watcher_task: Mutex::new(None),
            session_scan_wake: Notify::new(),
            shutdown_reason: AtomicU8::new(AppShutdownReason::NONE),
            shutdown_notify: Notify::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct UpdateManifest {
    schema_version: u32,
    version: String,
    tag: String,
    assets: Vec<UpdateManifestAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct UpdateManifestAsset {
    platform: String,
    arch: String,
    package_type: String,
    file_name: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UpdateCheck {
    current_version: String,
    latest_version: String,
    update_available: bool,
    selected_asset: Option<UpdateAssetInfo>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UpdateAssetInfo {
    platform: String,
    arch: String,
    package_type: String,
    file_name: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UpdateDownload {
    latest_version: String,
    file_path: String,
    file_name: String,
    size: u64,
    sha256: String,
    asset: UpdateAssetInfo,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaitingNotificationLedger {
    #[serde(default)]
    notification_keys: Vec<String>,
}

fn waiting_notification_ledger_path(store: &ConfigStore) -> PathBuf {
    store
        .path()
        .parent()
        .map(|parent| parent.join("webhook-notifications.json"))
        .unwrap_or_else(|| PathBuf::from("webhook-notifications.json"))
}

fn load_waiting_notification_ledger(path: &Path) -> HashSet<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    serde_json::from_str::<WaitingNotificationLedger>(&contents)
        .map(|ledger| {
            ledger
                .notification_keys
                .into_iter()
                .filter(|key| key.starts_with("waiting:"))
                .collect()
        })
        .unwrap_or_default()
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
    ignore_completed_before: i64,
}

type RecentSessionScanResult = (
    pending_approval::RecentSessionEventCache,
    RecentSessionEvents,
);
type RecentSessionScanTask = tokio::task::JoinHandle<RecentSessionScanResult>;

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
            .zip(&current.started_turns)
            .any(|(left, right)| {
                left.session_id != right.session_id || left.turn_id != right.turn_id
            })
        || previous.aborted_turns.len() != current.aborted_turns.len()
        || previous
            .aborted_turns
            .iter()
            .zip(&current.aborted_turns)
            .any(|(left, right)| {
                left.session_id != right.session_id || left.turn_id != right.turn_id
            })
        || previous.completed_turns.len() != current.completed_turns.len()
        || previous
            .completed_turns
            .iter()
            .zip(&current.completed_turns)
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
        for started in &events.started_turns {
            tracker
                .running
                .insert(terminal_turn_key(&started.session_id, &started.turn_id));
        }
        for completed in &events.completed_turns {
            tracker.mark_settled(completed);
        }
        for aborted in &events.aborted_turns {
            tracker.mark_terminal(&aborted.session_id, &aborted.turn_id);
        }
        tracker
    }

    fn completion_candidates(&mut self, events: &RecentSessionEvents) -> Vec<CompletedTurn> {
        for started in &events.started_turns {
            let key = terminal_turn_key(&started.session_id, &started.turn_id);
            if !self.settled.contains(&key) {
                self.running.insert(key);
            }
        }
        for aborted in &events.aborted_turns {
            self.mark_terminal(&aborted.session_id, &aborted.turn_id);
        }

        let mut candidates = Vec::new();
        let mut candidate_keys = HashSet::new();
        for completed in &events.completed_turns {
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
                self.settled.insert(key);
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
        self.settled.insert(key);
    }
}

fn unix_timestamp_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn initialize_waiting_notifications(path: &Path, baseline: HashSet<String>) -> HashSet<String> {
    let path_existed = path.exists();
    let mut initialized = if path_existed {
        load_waiting_notification_ledger(path)
    } else {
        HashSet::new()
    };
    let previous_len = initialized.len();
    initialized.extend(baseline);
    if !initialized.is_empty() && (!path_existed || initialized.len() != previous_len) {
        let _ = save_waiting_notification_ledger(path, &initialized);
    }
    initialized
}

fn initial_waiting_notifications(
    store: &ConfigStore,
    pending_approvals: &[pending_approval::PendingApproval],
) -> HashSet<String> {
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
    notification_keys: &HashSet<String>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "飞书通知记录路径无父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建飞书通知记录目录失败：{error}"))?;
    let mut notification_keys = notification_keys.iter().cloned().collect::<Vec<_>>();
    notification_keys.sort();
    let bytes = serde_json::to_vec_pretty(&WaitingNotificationLedger { notification_keys })
        .map_err(|error| format!("序列化飞书通知记录失败：{error}"))?;
    let temp = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&temp, bytes).map_err(|error| format!("写入飞书通知记录失败：{error}"))?;
    if let Err(error) = fs::rename(&temp, path) {
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)
                .map_err(|remove_error| format!("替换飞书通知记录失败：{remove_error}"))?;
            fs::rename(&temp, path)
                .map_err(|rename_error| format!("替换飞书通知记录失败：{rename_error}"))?;
        } else {
            return Err(format!("替换飞书通知记录失败：{error}"));
        }
        #[cfg(not(windows))]
        return Err(format!("替换飞书通知记录失败：{error}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("保护飞书通知记录失败：{error}"))?;
    }
    Ok(())
}

async fn persist_waiting_notification(
    state: &Arc<AppState>,
    notification_key: &str,
) -> Result<(), String> {
    let mut persisted = state.persisted_waiting_notifications.lock().await;
    if !persisted.insert(notification_key.to_string()) {
        return Ok(());
    }
    save_waiting_notification_ledger(&waiting_notification_ledger_path(&state.store), &persisted)
}

async fn baseline_waiting_notifications(
    state: &Arc<AppState>,
    pending_approvals: &[pending_approval::PendingApproval],
) {
    let baseline = waiting_notification_keys(pending_approvals);
    if baseline.is_empty() {
        return;
    }

    let persisted_snapshot = {
        let mut persisted = state.persisted_waiting_notifications.lock().await;
        let previous_len = persisted.len();
        persisted.extend(baseline.iter().cloned());
        (persisted.len() != previous_len).then(|| persisted.clone())
    };
    state.webhook_notifications.lock().await.extend(baseline);
    if let Some(persisted) = persisted_snapshot {
        let _ = save_waiting_notification_ledger(
            &waiting_notification_ledger_path(&state.store),
            &persisted,
        );
    }
}

impl AppState {
    pub fn request_shutdown(&self) {
        self.request_shutdown_with_reason(AppShutdownReason::CodexExited);
    }

    pub fn request_update_shutdown(&self) {
        self.request_shutdown_with_reason(AppShutdownReason::InstallUpdate);
    }

    fn request_shutdown_with_reason(&self, reason: AppShutdownReason) {
        if self
            .shutdown_reason
            .compare_exchange(
                AppShutdownReason::NONE,
                reason.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.shutdown_notify.notify_waiters();
        }
    }

    pub async fn wait_for_shutdown(&self) -> AppShutdownReason {
        if let Some(reason) =
            AppShutdownReason::from_u8(self.shutdown_reason.load(Ordering::Acquire))
        {
            return reason;
        }
        self.shutdown_notify.notified().await;
        AppShutdownReason::from_u8(self.shutdown_reason.load(Ordering::Acquire))
            .unwrap_or(AppShutdownReason::CodexExited)
    }

    pub async fn bridge_request(self: &Arc<Self>, path: String, payload: Value) -> Value {
        if let Some(command) = path.strip_prefix("/api/") {
            return invoke_api(self, command, payload).await;
        }
        match path.as_str() {
            "/settings/get" => serde_json::to_value(self.config.read().await.clone())
                .unwrap_or_else(|_| json!({"status":"failed"})),
            "/runtime/status" | "/backend/status" => {
                let mut value = runtime_status(self).await.unwrap_or_else(api_error_message);
                if let Some(object) = value.as_object_mut() {
                    object.insert("status".into(), Value::String("ok".into()));
                }
                value
            }
            "/session/wake-watcher" => {
                self.session_scan_wake.notify_one();
                json!({"status":"ok"})
            }
            "/session/titles" => cache_session_titles(self, &payload).await,
            "/thread-sort-keys" => {
                let sessions = payload
                    .get("sessions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|session| {
                        let session_id = session.get("session_id")?.as_str()?.trim();
                        if session_id.is_empty() {
                            return None;
                        }
                        Some(codex_plus_core::models::SessionRef {
                            session_id: session_id.to_string(),
                            title: session
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                    })
                    .collect::<Vec<_>>();
                session_metadata::thread_sort_keys(&codex_home(), &sessions)
            }
            "/session/delete" => {
                let session_id = payload
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let title = payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                delete_session_record(self, session_id, title)
                    .await
                    .unwrap_or_else(api_error_message)
            }
            "/session/export" => {
                let session_id = payload
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                session_transfer::export_session(&codex_home(), session_id)
                    .and_then(|result| serde_json::to_value(result).map_err(anyhow::Error::from))
                    .unwrap_or_else(|error| api_error_message(error.to_string()))
            }
            "/session/import" => {
                let project_path = payload
                    .get("projectPath")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let data = payload
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                session_transfer::import_session(&codex_home(), project_path, data)
                    .and_then(|result| serde_json::to_value(result).map_err(anyhow::Error::from))
                    .unwrap_or_else(|error| api_error_message(error.to_string()))
            }
            "/session/delete-messages" => {
                let session_id = payload
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let message_ids = payload
                    .get("messageIds")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                delete_selected_messages(self, session_id, message_ids)
                    .await
                    .unwrap_or_else(api_error_message)
            }
            "/webhook/session-completed" => notify_webhook_completion(self, &payload)
                .await
                .unwrap_or_else(api_error_message),
            "/webhook/session-waiting" => notify_webhook_waiting(self, &payload)
                .await
                .unwrap_or_else(api_error_message),
            "/plugins/list" => {
                let home = codex_home();
                match tokio::task::spawn_blocking(move || plugin_marketplace::list_plugins(&home))
                    .await
                {
                    Ok(result) => {
                        result.unwrap_or_else(|error| api_error_message(error.to_string()))
                    }
                    Err(error) => api_error_message(format!("插件列表任务异常退出：{error}")),
                }
            }
            "/plugins/status" => plugin_marketplace_status()
                .await
                .unwrap_or_else(api_error_message),
            "/plugins/repair" => repair_plugin_marketplace()
                .await
                .unwrap_or_else(api_error_message),
            _ => json!({"status":"failed","message":format!("未知 Codey 路由：{path}")}),
        }
    }
}

pub fn make_bridge_handler(state: &Arc<AppState>) -> codex_plus_core::bridge::BridgeHandler {
    let state_ref = Arc::clone(state);
    cdp::bridge_handler(move |path, payload| {
        let state_ref = state_ref.clone();
        async move { state_ref.bridge_request(path, payload).await }
    })
}

pub async fn invoke_api(state: &Arc<AppState>, command: &str, args: Value) -> Value {
    let result = match command {
        "load_codey_config" => load_codey_config(state).await,
        "save_codey_config" => match argument::<CodeyConfig>(&args, "config") {
            Ok(config) => save_codey_config(state, config).await,
            Err(error) => Err(error),
        },
        "pick_codex_app_directory" => pick_codex_app_directory().await,
        "set_codex_app_path" => match string_argument(&args, "path") {
            Ok(path) => set_codex_app_path(state, path).await,
            Err(error) => Err(error),
        },
        "sync_current_provider" => sync_current_provider_command(state).await,
        "fetch_current_provider_models" => fetch_current_provider_models(state).await,
        "save_selected_models" => match argument::<Vec<String>>(&args, "models") {
            Ok(models) => save_selected_models(state, models).await,
            Err(error) => Err(error),
        },
        "save_default_model" => match string_argument(&args, "model") {
            Ok(model) => save_default_model(state, model).await,
            Err(error) => Err(error),
        },
        "runtime_status" => runtime_status(state).await,
        "refresh_trace_log_stats" => refresh_trace_log_stats(state).await,
        "launch_codey" => launch_codey_runtime(state).await,
        "restart_codey" => schedule_restart_codey_runtime(state).await,
        "clear_codex_trace_logs" => clear_codex_trace_logs(state).await,
        "test_webhook" => test_webhook(state).await,
        "check_for_updates" => check_for_updates(state).await,
        "download_update" => download_update(state).await,
        "install_downloaded_update" => match string_argument(&args, "filePath") {
            Ok(file_path) => install_downloaded_update(state, file_path).await,
            Err(error) => Err(error),
        },
        "export_session" => match string_argument(&args, "sessionId") {
            Ok(session_id) => session_transfer::export_session(&codex_home(), &session_id)
                .and_then(|result| serde_json::to_value(result).map_err(anyhow::Error::from))
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "import_session" => {
            let project_path = string_argument(&args, "projectPath");
            let data = string_argument(&args, "data");
            match (project_path, data) {
                (Ok(project_path), Ok(data)) => {
                    session_transfer::import_session(&codex_home(), &project_path, &data)
                        .and_then(|result| {
                            serde_json::to_value(result).map_err(anyhow::Error::from)
                        })
                        .map_err(|error| error.to_string())
                }
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        }
        "delete_session" => {
            let session_id = string_argument(&args, "sessionId");
            let title = args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match session_id {
                Ok(session_id) => delete_session_record(state, session_id, title).await,
                Err(error) => Err(error),
            }
        }
        "delete_selected_messages" => {
            let session_id = string_argument(&args, "sessionId");
            let message_ids = argument::<Vec<String>>(&args, "messageIds");
            match (session_id, message_ids) {
                (Ok(session_id), Ok(message_ids)) => {
                    delete_selected_messages(state, session_id, message_ids).await
                }
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        }
        "plugin_marketplace_status" => plugin_marketplace_status().await,
        "repair_plugin_marketplace" => repair_plugin_marketplace().await,
        _ => Err(format!("未知 Codey API 命令：{command}")),
    };
    result.unwrap_or_else(api_error_message)
}

pub async fn load_codey_config(state: &Arc<AppState>) -> Result<Value, String> {
    let config = state.config.read().await.clone();
    let startup_error = state.startup_error.read().await.clone();
    #[cfg(windows)]
    let codex_app_path_selection_required =
        crate::launcher::needs_codex_app_path_selection(startup_error.as_deref());
    #[cfg(not(windows))]
    let codex_app_path_selection_required = false;
    let cc_switch = cc_switch::status_from_config(&config);
    let model_state = current_model_state(&config)?;
    let public_config = redacted_config(&config);
    Ok(json!({
        "config": public_config,
        "path": state.store.path().to_string_lossy(),
        "startupError": startup_error,
        "codexAppPathSelectionRequired": codex_app_path_selection_required,
        "ccSwitch": cc_switch,
        "modelState": model_state,
    }))
}

async fn pick_codex_app_directory() -> Result<Value, String> {
    #[cfg(windows)]
    {
        let selected = tokio::task::spawn_blocking(|| {
            rfd::FileDialog::new()
                .set_title("选择 Codex 桌面应用所在目录")
                .pick_folder()
        })
        .await
        .map_err(|error| format!("打开 Codex 目录选择器失败：{error}"))?;

        Ok(match selected {
            Some(path) => json!({
                "status": "selected",
                "path": path.to_string_lossy(),
            }),
            None => json!({"status": "cancelled"}),
        })
    }

    #[cfg(not(windows))]
    {
        Err("Codex 应用目录选择仅在 Windows 上提供".to_string())
    }
}

fn validate_codex_app_path(path: &str) -> Result<PathBuf, String> {
    let selected = path.trim();
    if selected.is_empty() {
        return Err("请先选择 Codex 桌面应用所在目录".to_string());
    }

    let app_dir = normalize_codex_app_path(Path::new(selected)).ok_or_else(|| {
        "所选目录不是可启动的 Codex 桌面应用。请选择包含 ChatGPT.exe 或 Codex.exe 的目录，不要选择 codex.exe 命令行程序".to_string()
    })?;
    let executable = build_codex_executable(&app_dir);
    if !executable.is_file() {
        return Err(format!(
            "所选目录中没有可启动的 Codex 桌面应用（未找到 {}）",
            executable.display()
        ));
    }
    Ok(app_dir)
}

async fn set_codex_app_path(state: &Arc<AppState>, path: String) -> Result<Value, String> {
    let app_dir = validate_codex_app_path(&path)?;
    let mut config = state.config.read().await.clone();
    config.codex_app_path = app_dir.to_string_lossy().to_string();
    save_codey_config(state, config).await
}

pub async fn save_codey_config(
    state: &Arc<AppState>,
    config_input: CodeyConfig,
) -> Result<Value, String> {
    let previous = state.config.read().await.clone();
    // Provider records, credentials and model-selection caches are read-only
    // through this general settings endpoint.
    let mut config = previous.clone();
    config.webhook = config_input.webhook;
    config.codex_app_path = config_input.codex_app_path;
    config.user_scripts = config_input.user_scripts;
    config.disable_trace_log_writes = config_input.disable_trace_log_writes;
    config.slim_codex_pet = config_input.slim_codex_pet;
    config.slim_codex_voice = config_input.slim_codex_voice;
    config.gpu_launch_mode = config_input.gpu_launch_mode;
    config.fast_context_tools = config_input.fast_context_tools;
    config.subagent_optimization = config_input.subagent_optimization;
    config.hide_full_access_warning = config_input.hide_full_access_warning;
    let config = config.normalize();
    let restart_required = runtime_config_requires_restart(state, &config).await;
    if config.disable_trace_log_writes != previous.disable_trace_log_writes {
        let home = codex_home();
        let disable_writes = config.disable_trace_log_writes;
        tokio::task::spawn_blocking(move || trace_log_guard::configure(&home, disable_writes))
            .await
            .map_err(|error| format!("Trace 日志保护切换任务异常退出：{error}"))?
            .map_err(|error| error.to_string())?;
    }
    state
        .store
        .save(&config)
        .map_err(|error| error.to_string())?;
    *state.config.write().await = config.clone();
    sync_waiting_webhook_watcher(state, &config).await;
    let cc_switch = cc_switch::status_from_config(&config);
    let model_state = current_model_state(&config)?;
    let public_config = redacted_config(&config);
    Ok(json!({
        "status":"ok",
        "config":public_config,
        "ccSwitch":cc_switch,
        "modelState":model_state,
        "restartRequired":restart_required,
    }))
}

pub async fn clear_codex_trace_logs(state: &Arc<AppState>) -> Result<Value, String> {
    let home = codex_home();
    let disable_writes = state.config.read().await.disable_trace_log_writes;
    let report = tokio::task::spawn_blocking(move || {
        trace_log_guard::configure(&home, disable_writes)?;
        trace_log_guard::clear(&home)
    })
    .await
    .map_err(|error| format!("Trace 日志库清理任务异常退出：{error}"))?
    .map_err(|error| error.to_string())?;
    Ok(json!({
        "status":"ok",
        "cleanup":report,
        "protectionEnabled":disable_writes,
    }))
}

pub async fn refresh_trace_log_stats(state: &Arc<AppState>) -> Result<Value, String> {
    if !state.trace_log_stats.begin_refresh() {
        return Ok(json!({
            "status": "pending",
            "traceLogStats": &state.trace_log_stats,
        }));
    }

    let home = codex_home();
    let snapshot = match tokio::task::spawn_blocking(move || trace_log_stats::snapshot(&home)).await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let mut snapshot = TraceLogStatsSnapshot::idle();
            snapshot
                .errors
                .push(format!("Trace 日志统计任务异常退出：{error}"));
            snapshot
        }
    };
    state.trace_log_stats.replace(snapshot);

    Ok(json!({
        "status": "ok",
        "traceLogStats": &state.trace_log_stats,
    }))
}

pub async fn sync_current_provider_command(state: &Arc<AppState>) -> Result<Value, String> {
    let cc_switch = sync_cc_switch_state(state).await;
    let config = state.config.read().await.clone();
    let restart_required = runtime_config_requires_restart(state, &config).await;
    let model_state = current_model_state(&config)?;
    let public_config = redacted_config(&config);
    Ok(json!({
        "status":"ok",
        "config":public_config,
        "ccSwitch":cc_switch,
        "modelState":model_state,
        "restartRequired":restart_required,
    }))
}

pub async fn sync_cc_switch_state(state: &Arc<AppState>) -> cc_switch::CcSwitchStatus {
    let previous = state.config.read().await.clone();
    match cc_switch::sync_current_provider(&previous, &codex_home()) {
        Ok((config, status)) => {
            if status.changed {
                if let Err(error) = state.store.save(&config) {
                    let mut failed = cc_switch::status_from_config(&previous);
                    failed.message = Some(format!("保存当前线路同步结果失败：{error}"));
                    return failed;
                }
                *state.config.write().await = config;
            }
            status
        }
        Err(error) => {
            let mut status = cc_switch::status_from_config(&previous);
            status.message = Some(error.to_string());
            status
        }
    }
}

pub async fn fetch_current_provider_models(state: &Arc<AppState>) -> Result<Value, String> {
    let config = state.config.read().await.clone();
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == config.active_profile_id)
        .cloned()
        .ok_or_else(|| "找不到当前线路".to_string())?;
    if profile.cc_switch_read_only {
        return Err("官方线路使用官方模型目录，无需同步第三方模型".to_string());
    }
    let models = provider_models::fetch(&profile, &state.http_client)
        .await
        .map_err(|error| error.to_string())?;
    let provider_id = config
        .current_provider_id()
        .ok_or_else(|| "当前线路缺少标识".to_string())?
        .to_string();
    let mut next = config;
    next.upstream_models_by_provider
        .insert(provider_id, models.clone());
    next = next.normalize();
    let model_state = current_model_state(&next)?;
    if should_refresh_model_catalog(&model_state) {
        refresh_model_catalog(&next)?;
    }
    state.store.save(&next).map_err(|error| error.to_string())?;
    *state.config.write().await = next.clone();
    let restart_required = runtime_config_requires_restart(state, &next).await;
    Ok(json!({
        "status":"ok",
        "models":models,
        "modelState":model_state,
        "restartRequired":restart_required,
    }))
}

pub async fn save_selected_models(
    state: &Arc<AppState>,
    requested_models: Vec<String>,
) -> Result<Value, String> {
    let mut config = state.config.read().await.clone();
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == config.active_profile_id)
        .ok_or_else(|| "找不到当前线路".to_string())?;
    if profile.cc_switch_read_only {
        return Err("官方线路不支持添加第三方模型".to_string());
    }
    let official = model_catalog::official_model_slugs(&codex_home())
        .map_err(|error| error.to_string())?
        .into_iter()
        .collect::<HashSet<_>>();
    let requested = requested_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
        .collect::<HashSet<_>>();
    let selected = config
        .upstream_models()
        .iter()
        .filter(|model| requested.contains(model.as_str()) && !official.contains(model.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let provider_id = config
        .current_provider_id()
        .ok_or_else(|| "当前线路缺少标识".to_string())?
        .to_string();
    if selected.is_empty() {
        config.selected_models_by_provider.remove(&provider_id);
    } else {
        config
            .selected_models_by_provider
            .insert(provider_id, selected);
    }
    config = config.normalize();
    refresh_model_catalog(&config)?;
    state
        .store
        .save(&config)
        .map_err(|error| error.to_string())?;
    *state.config.write().await = config.clone();
    let model_state = current_model_state(&config)?;
    let public_config = redacted_config(&config);
    let restart_required = runtime_config_requires_restart(state, &config).await;
    Ok(json!({
        "status":"ok",
        "config":public_config,
        "modelState":model_state,
        "restartRequired":restart_required,
    }))
}

pub async fn save_default_model(
    state: &Arc<AppState>,
    requested_model: String,
) -> Result<Value, String> {
    let mut config = state.config.read().await.clone();
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return Err("默认模型不能为空".to_string());
    }
    let model_state = current_model_state(&config)?;
    let supported = model_state
        .official_models
        .iter()
        .any(|model| model.supported && model.slug == requested_model)
        || model_state
            .third_party_models
            .iter()
            .any(|model| model == requested_model);
    if !supported {
        return Err(format!("模型 {requested_model} 当前不可用，无法设为默认"));
    }
    let provider_id = config
        .current_provider_id()
        .ok_or_else(|| "当前线路缺少标识".to_string())?
        .to_string();
    config
        .default_model_by_provider
        .insert(provider_id, requested_model.to_string());
    config = config.normalize();
    let restart_required = runtime_config_requires_restart(state, &config).await;
    state
        .store
        .save(&config)
        .map_err(|error| error.to_string())?;
    *state.config.write().await = config.clone();
    let model_state = current_model_state(&config)?;
    let public_config = redacted_config(&config);
    Ok(json!({
        "status":"ok",
        "config":public_config,
        "modelState":model_state,
        "restartRequired":restart_required,
    }))
}

fn redacted_config(config: &CodeyConfig) -> CodeyConfig {
    let mut public = config.clone();
    for profile in &mut public.profiles {
        profile.api_key.clear();
    }
    public
}

fn current_model_state(config: &CodeyConfig) -> Result<model_catalog::ModelSelectionState, String> {
    let official = config
        .profiles
        .iter()
        .find(|profile| profile.id == config.active_profile_id)
        .is_none_or(|profile| profile.cc_switch_read_only);
    Ok(model_catalog::selection_state(
        &codex_home(),
        official,
        config.upstream_models_snapshot(),
        config.selected_models(),
        config.default_model(),
    )
    .unwrap_or_default())
}

fn should_refresh_model_catalog(model_state: &model_catalog::ModelSelectionState) -> bool {
    !model_state.official_models.is_empty() || !model_state.third_party_models.is_empty()
}

fn refresh_model_catalog(config: &CodeyConfig) -> Result<(), String> {
    let official = config
        .profiles
        .iter()
        .find(|profile| profile.id == config.active_profile_id)
        .is_none_or(|profile| profile.cc_switch_read_only);
    model_catalog::refresh_for_provider(
        &codex_home(),
        official,
        config.upstream_models(),
        config.selected_models(),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn config_requires_restart(applied: &CodeyConfig, current: &CodeyConfig) -> bool {
    applied.active_profile() != current.active_profile()
        || applied.codex_app_path != current.codex_app_path
        || applied.user_scripts != current.user_scripts
        || applied.slim_codex_pet != current.slim_codex_pet
        || applied.slim_codex_voice != current.slim_codex_voice
        || applied.gpu_launch_mode != current.gpu_launch_mode
        || applied.fast_context_tools != current.fast_context_tools
        || applied.subagent_optimization != current.subagent_optimization
        || applied.selected_models() != current.selected_models()
        || applied.upstream_models() != current.upstream_models()
        || applied.default_model() != current.default_model()
}

async fn runtime_config_requires_restart(state: &Arc<AppState>, current: &CodeyConfig) -> bool {
    state
        .runtime
        .lock()
        .await
        .as_ref()
        .is_some_and(|runtime| config_requires_restart(&runtime.applied_config, current))
}

pub async fn runtime_status(state: &Arc<AppState>) -> Result<Value, String> {
    let runtime = state.runtime.lock().await.clone();
    let config = state.config.read().await;
    let profile = config.active_profile();
    let restart_required = runtime
        .as_ref()
        .is_some_and(|runtime| config_requires_restart(&runtime.applied_config, &config));
    let mut status = json!({
        "running": runtime.is_some(),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "clientPlatform": current_update_platform(),
        "activeProfileId": profile.as_ref().map(|profile| profile.id.as_str()).unwrap_or_default(),
        "activeProfileName": profile.as_ref().map(|profile| profile.name.as_str()).unwrap_or_default(),
        "restartRequired": restart_required,
        "restartInProgress": state.restart_in_progress.load(Ordering::Acquire),
    });
    drop(config);
    if let Some(error) = state.startup_error.read().await.clone()
        && let Some(object) = status.as_object_mut()
    {
        object.insert("startupError".into(), Value::String(error));
    };
    if let Some(runtime) = runtime.as_ref()
        && let Some(object) = status.as_object_mut()
    {
        object.insert(
            "codexAppPath".into(),
            Value::String(runtime.codex_app_path.to_string_lossy().to_string()),
        );
        object.insert(
            "maintenance".into(),
            serde_json::to_value(&runtime.maintenance).unwrap_or_else(|_| json!({})),
        );
    }
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "traceLogStats".into(),
            serde_json::to_value(&state.trace_log_stats).unwrap_or_else(|_| json!({})),
        );
    }
    Ok(status)
}

async fn launch_codey_inner(state: &Arc<AppState>) -> Result<Value, String> {
    let mut runtime_slot = state.runtime.lock().await;
    if runtime_slot.is_some() {
        return Ok(json!({"status":"already_running"}));
    }
    stop_waiting_webhook_watcher(state).await;
    restore_previous_runtime_state(&codex_home())
        .map_err(|error| format!("恢复上次 Codey 临时 Codex 配置失败：{error}"))?;
    let config = state.config.read().await.clone();
    let initial_scan_task = if webhook_watcher_should_run(&config) {
        let initial_event_cache = state
            .recent_session_event_cache
            .lock()
            .await
            .take()
            .unwrap_or_default();
        Some(start_recent_session_scan(initial_event_cache))
    } else {
        None
    };
    let handler = make_bridge_handler(state);
    let (runtime, codex_exit) = match CodeyRuntime::start(&config, handler).await {
        Ok(started) => started,
        Err(error) => {
            if let Some(initial_scan_task) = initial_scan_task {
                let (initial_event_cache, _) = await_recent_session_scan(initial_scan_task).await;
                *state.recent_session_event_cache.lock().await = Some(initial_event_cache);
            }
            return Err(error.to_string());
        }
    };
    *runtime_slot = Some(Arc::new(runtime));
    let runtime_generation = state.runtime_generation.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(initial_scan_task) = initial_scan_task {
        start_waiting_webhook_watcher(state, initial_scan_task).await;
    }
    drop(runtime_slot);
    let exit_state = Arc::clone(state);
    tokio::spawn(async move {
        if codex_exit.await.is_ok() {
            while exit_state.restart_in_progress.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            if exit_state.runtime_generation.load(Ordering::Acquire) == runtime_generation {
                exit_state.request_shutdown();
            }
        }
    });
    Ok(json!({"status":"running"}))
}

pub async fn launch_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    let result = launch_codey_inner(state).await;
    *state.startup_error.write().await = result.as_ref().err().cloned();
    result
}

pub async fn schedule_restart_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    if state.restart_in_progress.swap(true, Ordering::AcqRel) {
        return Ok(json!({"status":"already_restarting"}));
    }

    let restart_state = Arc::clone(state);
    tokio::spawn(async move {
        // The request originates inside the Codex renderer. Let the bridge
        // deliver its response before stopping the renderer that owns it.
        tokio::time::sleep(Duration::from_millis(250)).await;
        restart_state
            .runtime_generation
            .fetch_add(1, Ordering::AcqRel);
        if let Err(error) = stop_codey_runtime(&restart_state).await {
            *restart_state.startup_error.write().await = Some(error);
            restart_state
                .restart_in_progress
                .store(false, Ordering::Release);
            return;
        }
        if let Err(error) = launch_codey_runtime(&restart_state).await {
            eprintln!("Codey 自动重启 Codex 失败：{error}");
        }
        restart_state
            .restart_in_progress
            .store(false, Ordering::Release);
    });

    Ok(json!({"status":"restarting"}))
}

pub async fn stop_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    let mut runtime_slot = state.runtime.lock().await;
    stop_waiting_webhook_watcher(state).await;
    if let Some(runtime) = runtime_slot.take() {
        runtime.stop().await.map_err(|error| error.to_string())?;
    } else {
        restore_runtime_config(&codex_home()).map_err(|error| error.to_string())?;
    }
    *state.startup_error.write().await = None;
    Ok(json!({"status":"stopped"}))
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    #[test]
    fn restart_sensitive_config_changes_are_detected() {
        let applied = CodeyConfig::default();

        let mut model_change = applied.clone();
        let provider_id = model_change.current_provider_id().unwrap().to_string();
        model_change
            .selected_models_by_provider
            .insert(provider_id, vec!["third-party-model".into()]);
        assert!(config_requires_restart(&applied, &model_change));

        let mut startup_change = applied.clone();
        startup_change.slim_codex_voice = !startup_change.slim_codex_voice;
        assert!(config_requires_restart(&applied, &startup_change));

        let mut gpu_mode_change = applied.clone();
        gpu_mode_change.gpu_launch_mode = crate::config::GpuLaunchMode::DisableGpuRasterization;
        assert!(config_requires_restart(&applied, &gpu_mode_change));
    }

    #[test]
    fn live_config_changes_do_not_require_restart() {
        let applied = CodeyConfig::default();
        let mut current = applied.clone();
        current.webhook.enabled = true;
        current.webhook.url = "https://example.test/webhook".into();
        current.disable_trace_log_writes = !current.disable_trace_log_writes;

        assert!(!config_requires_restart(&applied, &current));
    }
}

fn webhook_watcher_should_run(config: &CodeyConfig) -> bool {
    config.webhook.enabled && !config.webhook.url.trim().is_empty()
}

async fn sync_waiting_webhook_watcher(state: &Arc<AppState>, config: &CodeyConfig) {
    let runtime_running = state.runtime.lock().await.is_some();
    if runtime_running && webhook_watcher_should_run(config) {
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

async fn start_waiting_webhook_watcher(
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
                        eprintln!("Codey 飞书完成通知失败：{error}");
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
            eprintln!("Codey 飞书等待通知失败：{error}");
        }
    }
}

async fn scan_recent_session_events(
    cache: pending_approval::RecentSessionEventCache,
) -> RecentSessionScanResult {
    await_recent_session_scan(start_recent_session_scan(cache)).await
}

fn start_recent_session_scan(
    mut cache: pending_approval::RecentSessionEventCache,
) -> RecentSessionScanTask {
    let home = codex_home();
    tokio::task::spawn_blocking(move || {
        let events = cache.refresh(&home);
        (cache, events)
    })
}

async fn await_recent_session_scan(scan_task: RecentSessionScanTask) -> RecentSessionScanResult {
    match scan_task.await {
        Ok(result) => result,
        Err(error) => {
            eprintln!("Codey 会话状态扫描任务异常退出：{error}");
            (
                pending_approval::RecentSessionEventCache::default(),
                RecentSessionEvents::default(),
            )
        }
    }
}

async fn stop_waiting_webhook_watcher(state: &Arc<AppState>) {
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

pub async fn test_webhook(state: &Arc<AppState>) -> Result<Value, String> {
    let webhook = state.config.read().await.webhook.clone();
    let dispatcher = WebhookDispatcher::with_client(state.http_client.clone(), webhook);
    dispatcher.test().await.map_err(|error| error.to_string())
}

pub async fn check_for_updates(state: &Arc<AppState>) -> Result<Value, String> {
    let manifest = fetch_configured_update_manifest(state).await?;
    let check = assess_update_manifest(env!("CARGO_PKG_VERSION"), &manifest)?;
    serde_json::to_value(check).map_err(|error| error.to_string())
}

pub async fn download_update(state: &Arc<AppState>) -> Result<Value, String> {
    let manifest = fetch_configured_update_manifest(state).await?;
    let check = assess_update_manifest(env!("CARGO_PKG_VERSION"), &manifest)?;
    if !check.update_available {
        return Err(format!("当前已是最新版本 v{}", check.current_version));
    }
    let asset = selected_update_asset(&manifest.assets)
        .ok_or_else(|| "没有适用于当前系统的可安装更新包".to_string())?;
    let file_path =
        download_update_asset(&state.http_client, &state.store, &manifest.version, &asset).await?;
    let download = UpdateDownload {
        latest_version: manifest.version,
        file_path: file_path.to_string_lossy().to_string(),
        file_name: asset.file_name.clone(),
        size: asset.size,
        sha256: asset.sha256.clone(),
        asset: asset_info(&asset),
    };
    serde_json::to_value(download).map_err(|error| error.to_string())
}

pub async fn install_downloaded_update(
    state: &Arc<AppState>,
    file_path: String,
) -> Result<Value, String> {
    let update_path = validate_downloaded_update_path(&state.store, &file_path)?;
    spawn_update_installer(&update_path)?;
    let shutdown_state = Arc::clone(state);
    tokio::spawn(async move {
        // Let the bridge deliver the response before Codex/Codey starts
        // normal shutdown and releases the executable for replacement.
        tokio::time::sleep(Duration::from_millis(250)).await;
        shutdown_state.request_update_shutdown();
    });
    Ok(json!({"status":"installing"}))
}

async fn fetch_configured_update_manifest(state: &Arc<AppState>) -> Result<UpdateManifest, String> {
    let manifest_url = state
        .config
        .read()
        .await
        .update_manifest_url
        .trim()
        .to_string();
    if manifest_url.is_empty() {
        return Err("内置更新地址未配置，请检查构建配置".to_string());
    }

    let url = reqwest::Url::parse(&manifest_url)
        .map_err(|_| "更新地址必须是有效的 HTTPS URL".to_string())?;
    if url.scheme() != "https" {
        return Err("更新地址必须使用 HTTPS".to_string());
    }

    let response = state
        .http_client
        .get(url)
        .header(
            USER_AGENT,
            format!("Codey/{} update-check", env!("CARGO_PKG_VERSION")),
        )
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| format!("检查更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("更新地址返回异常：{error}"))?;
    if response.url().scheme() != "https" {
        return Err("更新地址重定向到了非 HTTPS 地址".to_string());
    }

    let manifest = response
        .json::<UpdateManifest>()
        .await
        .map_err(|error| format!("更新清单格式无效：{error}"))?;
    Ok(manifest)
}

fn assess_update_manifest(
    current_version: &str,
    manifest: &UpdateManifest,
) -> Result<UpdateCheck, String> {
    if manifest.schema_version != 1 {
        return Err(format!("不支持的更新清单版本：{}", manifest.schema_version));
    }
    if manifest.tag != format!("v{}", manifest.version) {
        return Err("更新清单的版本和标签不一致".to_string());
    }
    if manifest.assets.is_empty() {
        return Err("更新清单没有可下载的安装包".to_string());
    }
    for asset in &manifest.assets {
        validate_update_asset(asset)?;
    }

    let current =
        Version::parse(current_version).map_err(|error| format!("当前版本格式无效：{error}"))?;
    let latest = Version::parse(&manifest.version)
        .map_err(|error| format!("更新清单版本格式无效：{error}"))?;
    Ok(UpdateCheck {
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        update_available: latest > current,
        selected_asset: selected_update_asset(&manifest.assets).map(|asset| asset_info(&asset)),
    })
}

fn validate_update_asset(asset: &UpdateManifestAsset) -> Result<(), String> {
    if asset.platform.trim().is_empty()
        || asset.arch.trim().is_empty()
        || asset.package_type.trim().is_empty()
        || asset.file_name.trim().is_empty()
        || asset.size == 0
    {
        return Err("更新清单包含不完整的安装包信息".to_string());
    }
    if asset.file_name.contains(['/', '\\'])
        || Path::new(&asset.file_name).components().count() != 1
    {
        return Err(format!("安装包文件名无效：{}", asset.file_name));
    }
    let url = reqwest::Url::parse(&asset.url)
        .map_err(|_| format!("安装包地址无效：{}", asset.file_name))?;
    if url.scheme() != "https" {
        return Err(format!("安装包地址必须使用 HTTPS：{}", asset.file_name));
    }
    if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("安装包 SHA-256 无效：{}", asset.file_name));
    }
    Ok(())
}

fn current_update_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        std::env::consts::OS
    }
}

fn current_update_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    }
}

fn installable_package_priority(asset: &UpdateManifestAsset) -> Option<u8> {
    match (
        current_update_platform(),
        asset.package_type.trim().to_ascii_lowercase().as_str(),
    ) {
        ("windows", "nsis") => Some(0),
        ("macos", "app-zip") => Some(0),
        _ => None,
    }
}

fn selected_update_asset(assets: &[UpdateManifestAsset]) -> Option<UpdateManifestAsset> {
    let platform = current_update_platform();
    let arch = current_update_arch();
    assets
        .iter()
        .filter(|asset| {
            asset.platform.eq_ignore_ascii_case(platform)
                && asset.arch.eq_ignore_ascii_case(arch)
                && installable_package_priority(asset).is_some()
        })
        .min_by_key(|asset| installable_package_priority(asset).unwrap_or(u8::MAX))
        .cloned()
}

fn asset_info(asset: &UpdateManifestAsset) -> UpdateAssetInfo {
    UpdateAssetInfo {
        platform: asset.platform.clone(),
        arch: asset.arch.clone(),
        package_type: asset.package_type.clone(),
        file_name: asset.file_name.clone(),
        url: asset.url.clone(),
        sha256: asset.sha256.clone(),
        size: asset.size,
    }
}

fn update_download_dir(store: &ConfigStore) -> Result<PathBuf, String> {
    let parent = store
        .path()
        .parent()
        .ok_or_else(|| "Codey 配置路径无父目录，无法创建更新缓存".to_string())?;
    Ok(parent.join("updates"))
}

async fn download_update_asset(
    client: &reqwest::Client,
    store: &ConfigStore,
    version: &str,
    asset: &UpdateManifestAsset,
) -> Result<PathBuf, String> {
    validate_update_asset(asset)?;
    let url = reqwest::Url::parse(&asset.url)
        .map_err(|_| format!("安装包地址无效：{}", asset.file_name))?;
    let directory = update_download_dir(store)?.join(format!("v{version}"));
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| format!("创建更新缓存目录失败：{error}"))?;
    let destination = directory.join(&asset.file_name);
    let temporary = directory.join(format!(".{}.download", asset.file_name));
    let _ = tokio::fs::remove_file(&temporary).await;

    let response = client
        .get(url)
        .header(
            USER_AGENT,
            format!("Codey/{} update-download", env!("CARGO_PKG_VERSION")),
        )
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|error| format!("下载更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("下载安装包失败：{error}"))?;
    if response.url().scheme() != "https" {
        return Err("安装包地址重定向到了非 HTTPS 地址".to_string());
    }

    let mut file = tokio::fs::File::create(&temporary)
        .await
        .map_err(|error| format!("创建更新缓存文件失败：{error}"))?;
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    let mut bytes_written = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取更新下载数据失败：{error}"))?;
        bytes_written += chunk.len() as u64;
        if bytes_written > asset.size {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err("下载的安装包大小超过更新清单声明".to_string());
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("写入更新缓存文件失败：{error}"))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("刷新更新缓存文件失败：{error}"))?;
    drop(file);

    if bytes_written != asset.size {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!(
            "安装包大小不一致：期望 {} 字节，实际 {} 字节",
            asset.size, bytes_written
        ));
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(&asset.sha256) {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err("安装包 SHA-256 校验失败".to_string());
    }
    let _ = tokio::fs::remove_file(&destination).await;
    tokio::fs::rename(&temporary, &destination)
        .await
        .map_err(|error| format!("保存更新安装包失败：{error}"))?;
    Ok(destination)
}

fn validate_downloaded_update_path(
    store: &ConfigStore,
    file_path: &str,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(file_path);
    if !path.is_absolute() {
        return Err("更新安装包路径必须是绝对路径".to_string());
    }
    let root = update_download_dir(store)?
        .canonicalize()
        .map_err(|error| format!("读取更新缓存目录失败：{error}"))?;
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("读取更新安装包失败：{error}"))?;
    if !canonical.starts_with(&root) {
        return Err("只能安装 Codey 下载缓存中的更新包".to_string());
    }
    Ok(canonical)
}

#[cfg(target_os = "windows")]
fn spawn_update_installer(update_path: &Path) -> Result<(), String> {
    crate::update_helper::spawn_update_installer(update_path)
}

#[cfg(target_os = "macos")]
fn spawn_update_installer(update_path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    if !update_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        return Err("macOS 更新安装包必须是 .zip".to_string());
    }
    let app_bundle = current_macos_app_bundle()
        .ok_or_else(|| "当前 Codey 不是从 .app 包运行，无法自动替换".to_string())?;
    let script_path = update_path
        .parent()
        .ok_or_else(|| "更新安装包路径无父目录".to_string())?
        .join("install-codey-update.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
set -eu
parent_pid="$1"
archive="$2"
app_bundle="$3"
app_parent="$(dirname "$app_bundle")"
app_name="$(basename "$app_bundle")"
while kill -0 "$parent_pid" 2>/dev/null; do
  sleep 0.2
done
tmp_dir="$app_parent/.${app_name}.codey-update.$$"
rm -rf "$tmp_dir"
mkdir -p "$tmp_dir"
/usr/bin/ditto -x -k "$archive" "$tmp_dir"
test -d "$tmp_dir/$app_name"
rm -rf "$app_bundle"
mv "$tmp_dir/$app_name" "$app_bundle"
rm -rf "$tmp_dir"
/usr/bin/open "$app_bundle"
"#,
    )
    .map_err(|error| format!("写入更新安装脚本失败：{error}"))?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("设置更新安装脚本权限失败：{error}"))?;
    std::process::Command::new("/bin/sh")
        .arg(&script_path)
        .arg(std::process::id().to_string())
        .arg(update_path)
        .arg(app_bundle)
        .spawn()
        .map_err(|error| format!("启动更新安装脚本失败：{error}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn current_macos_app_bundle() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .ancestors()
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("app"))
        .map(Path::to_path_buf)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn spawn_update_installer(_update_path: &Path) -> Result<(), String> {
    Err("当前平台暂不支持自动安装更新".to_string())
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

async fn cache_session_titles(state: &Arc<AppState>, payload: &Value) -> Value {
    let Some(titles) = payload.get("titles").and_then(Value::as_array) else {
        return api_error_message("会话标题同步缺少 titles");
    };
    let mut cached = state.session_titles.write().await;
    for title in titles {
        let session_id = title
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .trim_start_matches("local:");
        let session_name = title
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if session_id.is_empty() || session_name.is_empty() {
            continue;
        }
        if cached.len() >= 4096 && !cached.contains_key(session_id) {
            cached.clear();
        }
        cached.insert(session_id.to_string(), session_name.to_string());
    }
    json!({"status":"ok"})
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
    session_metadata::resolve_session_name_with_preferred(
        &codex_home(),
        session_id,
        cached_title.as_deref(),
    )
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
    let sent = state.webhook_notifications.lock().await;
    keys.iter().any(|key| sent.contains(key))
}

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
    let webhook = state.config.read().await.webhook.clone();
    if !webhook.enabled || webhook.url.trim().is_empty() {
        return Ok(json!({"status":"skipped","reason":"disabled"}));
    }

    let [completed_key, failed_key] = terminal_notification_keys(&session_id, &turn_id);
    {
        let mut sent = state.webhook_notifications.lock().await;
        if sent.contains(&completed_key) || sent.contains(&failed_key) {
            return Ok(json!({"status":"duplicate"}));
        }
        if sent.len() >= 2048 {
            sent.clear();
        }
        sent.insert(failed_key.clone());
    }
    let dispatcher = WebhookDispatcher::with_client(state.http_client.clone(), webhook);
    let session_name = webhook_session_name(state, payload, &session_id).await;
    let event = WebhookEvent::new(
        "session.failed",
        session_id,
        profile_id,
        model,
        duration_ms,
        Some(error),
    )
    .with_session_name(session_name)
    .with_reasoning_effort(reasoning_effort);
    if let Err(error) = dispatcher.send(&event).await {
        state.webhook_notifications.lock().await.remove(&failed_key);
        return Err(error.to_string());
    }
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
        return Err("飞书任务完成通知缺少会话或轮次 ID".to_string());
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
    let (webhook, profile_id) = {
        let config = state.config.read().await;
        (
            config.webhook.clone(),
            config
                .active_profile()
                .map(|profile| profile.id)
                .unwrap_or_default(),
        )
    };
    if !webhook.enabled || webhook.url.trim().is_empty() {
        return Ok(json!({"status":"skipped","reason":"disabled"}));
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
        let mut sent = state.webhook_notifications.lock().await;
        let failed_key = format!("failed:{session_id}:{turn_id}");
        if sent.contains(&notification_key) || sent.contains(&failed_key) {
            return Ok(json!({"status":"duplicate"}));
        }
        if sent.len() >= 2048 {
            sent.clear();
        }
        sent.insert(notification_key.clone());
    }
    let dispatcher = WebhookDispatcher::with_client(state.http_client.clone(), webhook);
    let session_name = webhook_session_name(state, payload, &session_id).await;
    let event = WebhookEvent::new(
        "session.completed",
        session_id,
        profile_id,
        model,
        requested_duration_ms,
        None,
    )
    .with_session_name(session_name)
    .with_reasoning_effort(reasoning_effort);
    if let Err(error) = dispatcher.send(&event).await {
        state
            .webhook_notifications
            .lock()
            .await
            .remove(&notification_key);
        return Err(error.to_string());
    }
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
        return Err("飞书等待介入通知缺少会话 ID".to_string());
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
    let (webhook, profile_id) = {
        let config = state.config.read().await;
        (
            config.webhook.clone(),
            config
                .active_profile()
                .map(|profile| profile.id)
                .unwrap_or_default(),
        )
    };
    if !webhook.enabled || webhook.url.trim().is_empty() {
        return Ok(json!({"status":"skipped","reason":"disabled"}));
    }

    let notification_key = waiting_notification_key(&session_id, turn_id, waiting_id);
    if state
        .persisted_waiting_notifications
        .lock()
        .await
        .contains(&notification_key)
    {
        return Ok(json!({"status":"duplicate"}));
    }
    {
        let mut sent = state.webhook_notifications.lock().await;
        if sent.contains(&notification_key) {
            return Ok(json!({"status":"duplicate"}));
        }
        if sent.len() >= 2048 {
            sent.clear();
        }
        sent.insert(notification_key.clone());
    }
    let dispatcher = WebhookDispatcher::with_client(state.http_client.clone(), webhook);
    let session_name = webhook_session_name(state, payload, &session_id).await;
    let event = WebhookEvent::new(
        "session.waiting",
        session_id,
        profile_id,
        model,
        duration_ms,
        None,
    )
    .with_session_name(session_name)
    .with_reasoning_effort(reasoning_effort);
    if let Err(error) = dispatcher.send(&event).await {
        state
            .webhook_notifications
            .lock()
            .await
            .remove(&notification_key);
        return Err(error.to_string());
    }
    persist_waiting_notification(state, &notification_key).await?;
    Ok(json!({"status":"ok","eventId":event.event_id}))
}

pub async fn delete_selected_messages(
    _state: &Arc<AppState>,
    session_id: String,
    message_ids: Vec<String>,
) -> Result<Value, String> {
    let home = codex_home();
    let result =
        tokio::task::spawn_blocking(move || delete_messages(&home, &session_id, &message_ids))
            .await
            .map_err(|error| format!("消息删除任务异常退出：{error}"))?
            .map_err(|error| error.to_string())?;
    serde_json::to_value(result).map_err(|error| error.to_string())
}

pub async fn delete_session_record(
    state: &Arc<AppState>,
    session_id: String,
    title: String,
) -> Result<Value, String> {
    let backup_root = state
        .store
        .path()
        .parent()
        .map(|parent| parent.join("session-backups"))
        .unwrap_or_else(|| PathBuf::from(".codey/session-backups"));
    let home = codex_home();
    let result = tokio::task::spawn_blocking(move || {
        session_delete::delete_session(&home, &backup_root, &session_id, &title)
    })
    .await
    .map_err(|error| format!("会话删除任务异常退出：{error}"))?
    .map_err(|error| error.to_string())?;
    let normalized_session_id = result.session_id.trim_start_matches("local:").to_string();
    state
        .session_titles
        .write()
        .await
        .remove(&normalized_session_id);
    Ok(json!({
        "status": "ok",
        "deleted": true,
        "sessionId": normalized_session_id,
        "message": result.message,
    }))
}

pub async fn plugin_marketplace_status() -> Result<Value, String> {
    let home = codex_home();
    let marketplace_home = home.clone();
    let mut status = tokio::task::spawn_blocking(move || {
        plugin_marketplace::marketplaces_status(&marketplace_home)
    })
    .await
    .map_err(|error| format!("插件市场状态任务异常退出：{error}"))?;
    decorate_plugin_marketplace_status(&home, &mut status);
    Ok(status)
}

pub async fn repair_plugin_marketplace() -> Result<Value, String> {
    let home = codex_home();
    let marketplace_home = home.clone();
    let repair = tokio::task::spawn_blocking(move || {
        plugin_marketplace::ensure_marketplaces(&marketplace_home)
    })
    .await
    .map_err(|error| format!("插件市场修复任务异常退出：{error}"))?
    .map_err(|error| error.to_string())?;
    let mut status = plugin_marketplace::marketplaces_status(&home);
    if let Some(object) = status.as_object_mut() {
        for key in ["initializedRemote", "configuredRemote", "configChanged"] {
            if let Some(value) = repair.get(key) {
                object.insert(key.into(), value.clone());
            }
        }
    }
    decorate_plugin_marketplace_status(&home, &mut status);
    Ok(status)
}

fn decorate_plugin_marketplace_status(home: &Path, status: &mut Value) {
    let needs_repair = status
        .get("needsRepair")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "status".into(),
            Value::String(
                if needs_repair {
                    "needs_repair"
                } else {
                    "ready"
                }
                .into(),
            ),
        );
        object.insert(
            "localMarketplacePath".into(),
            Value::String(home.join(".tmp/plugins").to_string_lossy().to_string()),
        );
    }
}

fn argument<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(
        args.get(name)
            .cloned()
            .ok_or_else(|| format!("缺少参数：{name}"))?,
    )
    .map_err(|error| format!("参数 {name} 无效：{error}"))
}

fn string_argument(args: &Value, name: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("缺少参数：{name}"))
}

fn api_error_message(error: impl ToString) -> Value {
    json!({"status":"failed","message":error.to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_approval::{AbortedTurn, PendingApproval, StartedTurn};

    #[test]
    fn model_sync_can_defer_catalog_refresh_until_a_model_is_selectable() {
        assert!(!should_refresh_model_catalog(
            &model_catalog::ModelSelectionState::default()
        ));

        let mut state = model_catalog::ModelSelectionState::default();
        state.third_party_models.push("provider-model".to_string());
        assert!(should_refresh_model_catalog(&state));
    }

    #[test]
    fn selected_codex_app_path_requires_a_desktop_executable() {
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_codex_app_path(directory.path().to_str().unwrap()).is_err());

        let executable = directory.path().join("Codex.exe");
        fs::write(&executable, []).unwrap();
        assert_eq!(
            validate_codex_app_path(directory.path().to_str().unwrap()).unwrap(),
            directory.path()
        );
    }

    #[test]
    fn update_manifest_reports_a_newer_https_release() {
        let manifest = serde_json::from_value::<UpdateManifest>(json!({
            "schema_version": 1,
            "version": "0.2.0",
            "tag": "v0.2.0",
            "assets": [{
                "platform": "windows",
                "arch": "x64",
                "package_type": "nsis",
                "file_name": "Codey-0.2.0-windows-x64-setup.exe",
                "url": "https://updates.example.com/releases/v0.2.0/Codey-0.2.0-windows-x64-setup.exe",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size": 1024
            }]
        }))
        .unwrap();

        let result = assess_update_manifest("0.1.0", &manifest).unwrap();

        assert_eq!(result.current_version, "0.1.0");
        assert_eq!(result.latest_version, "0.2.0");
        assert!(result.update_available);
    }

    #[test]
    fn update_manifest_selects_the_current_platform_installer() {
        let platform = current_update_platform();
        let arch = current_update_arch();
        let package_type = if platform == "windows" {
            "nsis"
        } else {
            "app-zip"
        };
        let file_name = if platform == "windows" {
            "Codey-0.2.0-windows-x64-setup.exe"
        } else {
            "Codey-0.2.0-macos-arm64-unsigned.zip"
        };
        let manifest = serde_json::from_value::<UpdateManifest>(json!({
            "schema_version": 1,
            "version": "0.2.0",
            "tag": "v0.2.0",
            "assets": [
                {
                    "platform": "windows",
                    "arch": "x64",
                    "package_type": "portable-zip",
                    "file_name": "Codey-0.2.0-windows-x64-portable.zip",
                    "url": "https://updates.example.com/releases/v0.2.0/Codey-0.2.0-windows-x64-portable.zip",
                    "sha256": "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size": 1024
                },
                {
                    "platform": platform,
                    "arch": arch,
                    "package_type": package_type,
                    "file_name": file_name,
                    "url": format!("https://updates.example.com/releases/v0.2.0/{file_name}"),
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size": 2048
                }
            ]
        }))
        .unwrap();

        let result = assess_update_manifest("0.1.0", &manifest).unwrap();

        assert_eq!(
            result
                .selected_asset
                .as_ref()
                .map(|asset| asset.package_type.as_str()),
            Some(package_type)
        );
        assert_eq!(
            result
                .selected_asset
                .as_ref()
                .map(|asset| asset.arch.as_str()),
            Some(arch)
        );
    }

    #[tokio::test]
    async fn app_state_preserves_update_shutdown_reason() {
        let state = AppState::default();

        state.request_update_shutdown();

        assert_eq!(
            state.wait_for_shutdown().await,
            AppShutdownReason::InstallUpdate
        );
    }

    #[test]
    fn update_manifest_rejects_insecure_asset_urls() {
        let manifest = serde_json::from_value::<UpdateManifest>(json!({
            "schema_version": 1,
            "version": "0.2.0",
            "tag": "v0.2.0",
            "assets": [{
                "platform": "windows",
                "arch": "x64",
                "package_type": "nsis",
                "file_name": "Codey-0.2.0-windows-x64-setup.exe",
                "url": "http://updates.example.com/Codey-0.2.0-windows-x64-setup.exe",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size": 1024
            }]
        }))
        .unwrap();

        assert!(
            assess_update_manifest("0.1.0", &manifest)
                .unwrap_err()
                .contains("必须使用 HTTPS")
        );
    }

    #[test]
    fn update_manifest_rejects_asset_path_traversal() {
        let manifest = serde_json::from_value::<UpdateManifest>(json!({
            "schema_version": 1,
            "version": "0.2.0",
            "tag": "v0.2.0",
            "assets": [{
                "platform": "windows",
                "arch": "x64",
                "package_type": "nsis",
                "file_name": "../Codey.exe",
                "url": "https://updates.example.com/Codey.exe",
                "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "size": 1024
            }]
        }))
        .unwrap();

        assert!(
            assess_update_manifest("0.1.0", &manifest)
                .unwrap_err()
                .contains("文件名无效")
        );
    }

    fn lifecycle(started: &[(&str, &str)], completed: &[(&str, &str)]) -> RecentSessionEvents {
        RecentSessionEvents {
            started_turns: started
                .iter()
                .map(|(session_id, turn_id)| StartedTurn {
                    session_id: (*session_id).to_string(),
                    turn_id: (*turn_id).to_string(),
                })
                .collect(),
            completed_turns: completed
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
            ..RecentSessionEvents::default()
        }
    }

    #[test]
    fn webhook_notifications_use_the_matching_turn_configuration() {
        let events = RecentSessionEvents {
            turn_configurations: HashMap::from([(
                "session-1".to_string(),
                HashMap::from([(
                    "turn-1".to_string(),
                    pending_approval::TurnConfiguration {
                        model: "gpt-5.6-luna".to_string(),
                        reasoning_effort: "xhigh".to_string(),
                    },
                )]),
            )]),
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
            session_statuses: HashMap::from([(
                "session-1".to_string(),
                SessionLifecycleStatus::Running,
            )]),
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

        save_waiting_notification_ledger(&path, &expected).unwrap();

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

        assert_eq!(
            initialize_waiting_notifications(&path, baseline.clone()),
            baseline
        );
        assert_eq!(load_waiting_notification_ledger(&path), baseline);
    }

    #[test]
    fn every_startup_merges_all_existing_waits_into_the_baseline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("webhook-notifications.json");
        let persisted = HashSet::from(["waiting:session-sent:turn-sent:approval-sent".to_string()]);
        save_waiting_notification_ledger(&path, &persisted).unwrap();
        let existing_wait = "waiting:session-old:turn-old:approval-old".to_string();

        let loaded =
            initialize_waiting_notifications(&path, HashSet::from([existing_wait.clone()]));

        let expected = persisted
            .into_iter()
            .chain([existing_wait])
            .collect::<HashSet<_>>();
        assert_eq!(loaded, expected);
        assert_eq!(load_waiting_notification_ledger(&path), expected);
    }

    #[tokio::test]
    async fn startup_baseline_only_suppresses_waits_seen_before_runtime_start() {
        let directory = tempfile::tempdir().unwrap();
        let mut state = AppState::default();
        state.store = ConfigStore::new(directory.path().join("config.json"));
        state.webhook_notifications = Mutex::new(HashSet::new());
        state.persisted_waiting_notifications = Mutex::new(HashSet::new());
        let state = Arc::new(state);
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
        let state = Arc::new(AppState::default());
        *state.recent_session_event_cache.lock().await = None;
        let (release_scan_tx, release_scan_rx) = oneshot::channel();
        let initial_scan_task = tokio::spawn(async move {
            let _ = release_scan_rx.await;
            (
                pending_approval::RecentSessionEventCache::default(),
                RecentSessionEvents::default(),
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
    fn webhook_turn_tracker_fences_an_aborted_turn_from_late_completion() {
        let mut tracker = WebhookTurnTracker::default();
        let terminal = RecentSessionEvents {
            started_turns: vec![StartedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
            }],
            aborted_turns: vec![AbortedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
            }],
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
        imported.completed_turns[0].completed_at = Some(100);

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
        live.completed_turns[0].completed_at = Some(200);

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
        imported.completed_turns[0].completed_at = Some(300);
        imported.completed_turns[0].is_snapshot_replay = true;

        assert!(tracker.completion_candidates(&imported).is_empty());
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
