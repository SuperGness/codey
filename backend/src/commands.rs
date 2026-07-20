use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, RwLock, oneshot};

use crate::cc_switch;
use crate::cdp;
use crate::codex_config::codex_home;
use crate::config::{CodeyConfig, ConfigStore};
use crate::launcher::{CodeyRuntime, restore_previous_runtime_state, restore_runtime_config};
use crate::message_delete::delete_messages;
use crate::model_catalog;
use crate::pending_approval;
use crate::pending_approval::{CompletedTurn, RecentSessionEvents};
use crate::plugin_marketplace;
use crate::provider_models;
use crate::session_delete;
use crate::session_metadata;
use crate::session_transfer;
use crate::trace_log_guard;
use crate::webhook::{WebhookDispatcher, WebhookEvent};

pub struct AppState {
    pub store: ConfigStore,
    pub config: RwLock<CodeyConfig>,
    pub runtime: Mutex<Option<Arc<CodeyRuntime>>>,
    pub startup_error: RwLock<Option<String>>,
    session_titles: RwLock<HashMap<String, String>>,
    webhook_notifications: Mutex<HashSet<String>>,
    persisted_waiting_notifications: Mutex<HashSet<String>>,
    waiting_watcher_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    shutdown_requested: AtomicBool,
    shutdown_notify: Notify,
}

impl Default for AppState {
    fn default() -> Self {
        let store = ConfigStore::default();
        let config = store.load().unwrap_or_default();
        let persisted_waiting_notifications = initial_waiting_notifications(&store);
        Self {
            store,
            config: RwLock::new(config),
            runtime: Mutex::new(None),
            startup_error: RwLock::new(None),
            session_titles: RwLock::new(HashMap::new()),
            webhook_notifications: Mutex::new(persisted_waiting_notifications.clone()),
            persisted_waiting_notifications: Mutex::new(persisted_waiting_notifications),
            waiting_watcher_shutdown: Mutex::new(None),
            shutdown_requested: AtomicBool::new(false),
            shutdown_notify: Notify::new(),
        }
    }
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

fn initial_waiting_notifications(store: &ConfigStore) -> HashSet<String> {
    let path = waiting_notification_ledger_path(store);
    let baseline = pending_approval::recent_pending_approvals(&codex_home())
        .into_iter()
        .map(|pending| {
            waiting_notification_key(
                pending.session_id.trim_start_matches("local:"),
                &pending.turn_id,
                &pending.waiting_id,
            )
        })
        .collect();
    initialize_waiting_notifications(&path, baseline)
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

impl AppState {
    pub fn request_shutdown(&self) {
        if !self.shutdown_requested.swap(true, Ordering::AcqRel) {
            self.shutdown_notify.notify_waiters();
        }
    }

    pub async fn wait_for_shutdown(&self) {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return;
        }
        self.shutdown_notify.notified().await;
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
            "/session/titles" => cache_session_titles(self, &payload).await,
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
            "/plugins/list" => plugin_marketplace::list_plugins(&codex_home())
                .unwrap_or_else(|error| api_error_message(error.to_string())),
            "/plugins/status" => plugin_marketplace_status()
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
        "sync_current_provider" => sync_current_provider_command(state).await,
        "fetch_current_provider_models" => fetch_current_provider_models(state).await,
        "save_selected_models" => match argument::<Vec<String>>(&args, "models") {
            Ok(models) => save_selected_models(state, models).await,
            Err(error) => Err(error),
        },
        "runtime_status" => runtime_status(state).await,
        "launch_codey" => launch_codey_runtime(state).await,
        "clear_codex_trace_logs" => clear_codex_trace_logs(state).await,
        "test_webhook" => test_webhook(state).await,
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
        _ => Err(format!("未知 Codey API 命令：{command}")),
    };
    result.unwrap_or_else(api_error_message)
}

pub async fn load_codey_config(state: &Arc<AppState>) -> Result<Value, String> {
    let config = state.config.read().await.clone();
    let startup_error = state.startup_error.read().await.clone();
    let cc_switch = cc_switch::status_from_config(&config);
    let model_state = current_model_state(&config)?;
    let public_config = redacted_config(&config);
    Ok(json!({
        "config": public_config,
        "path": state.store.path().to_string_lossy(),
        "startupError": startup_error,
        "ccSwitch": cc_switch,
        "modelState": model_state,
    }))
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
    config.fast_context_tools = config_input.fast_context_tools;
    let config = config.normalize();
    let restart_required = config.fast_context_tools != previous.fast_context_tools
        && state.runtime.lock().await.is_some();
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

pub async fn sync_current_provider_command(state: &Arc<AppState>) -> Result<Value, String> {
    let previous_id = state
        .config
        .read()
        .await
        .current_provider_id()
        .map(ToString::to_string);
    let cc_switch = sync_cc_switch_state(state).await;
    let config = state.config.read().await.clone();
    let changed_provider = previous_id.as_deref() != config.current_provider_id();
    let restart_required = changed_provider && state.runtime.lock().await.is_some();
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
    let models = provider_models::fetch(&profile)
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
    refresh_model_catalog(&next)?;
    state.store.save(&next).map_err(|error| error.to_string())?;
    *state.config.write().await = next.clone();
    let model_state = current_model_state(&next)?;
    Ok(json!({"status":"ok","models":models,"modelState":model_state}))
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
    Ok(json!({"status":"ok","config":public_config,"modelState":model_state}))
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
        config.upstream_models(),
        config.selected_models(),
    )
    .unwrap_or_default())
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

pub async fn runtime_status(state: &Arc<AppState>) -> Result<Value, String> {
    let runtime = state.runtime.lock().await.clone();
    let config = state.config.read().await;
    let profile = config.active_profile();
    let mut status = json!({
        "running": runtime.is_some(),
        "activeProfileId": profile.as_ref().map(|profile| profile.id.as_str()).unwrap_or_default(),
        "activeProfileName": profile.as_ref().map(|profile| profile.name.as_str()).unwrap_or_default(),
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
        object.insert(
            "traceLogStats".into(),
            serde_json::to_value(&runtime.trace_log_stats).unwrap_or_else(|_| json!({})),
        );
    }
    Ok(status)
}

async fn launch_codey_inner(state: &Arc<AppState>) -> Result<Value, String> {
    let mut runtime_slot = state.runtime.lock().await;
    if runtime_slot.is_some() {
        return Ok(json!({"status":"already_running"}));
    }
    restore_previous_runtime_state(&codex_home())
        .map_err(|error| format!("恢复上次 Codey 临时 Codex 配置失败：{error}"))?;
    let config = state.config.read().await.clone();
    let handler = make_bridge_handler(state);
    let (runtime, codex_exit) = CodeyRuntime::start(&config, handler)
        .await
        .map_err(|error| error.to_string())?;
    *runtime_slot = Some(Arc::new(runtime));
    drop(runtime_slot);
    start_waiting_webhook_watcher(state).await;
    let exit_state = Arc::clone(state);
    tokio::spawn(async move {
        if codex_exit.await.is_ok() {
            exit_state.request_shutdown();
        }
    });
    Ok(json!({"status":"running"}))
}

pub async fn launch_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    let result = launch_codey_inner(state).await;
    *state.startup_error.write().await = result.as_ref().err().cloned();
    result
}

pub async fn stop_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    stop_waiting_webhook_watcher(state).await;
    let mut runtime_slot = state.runtime.lock().await;
    if let Some(runtime) = runtime_slot.take() {
        runtime.stop().await.map_err(|error| error.to_string())?;
    } else {
        restore_runtime_config(&codex_home()).map_err(|error| error.to_string())?;
    }
    *state.startup_error.write().await = None;
    Ok(json!({"status":"stopped"}))
}

async fn start_waiting_webhook_watcher(state: &Arc<AppState>) {
    stop_waiting_webhook_watcher(state).await;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    *state.waiting_watcher_shutdown.lock().await = Some(shutdown_tx);
    let debug_port = state
        .runtime
        .lock()
        .await
        .as_ref()
        .map(|runtime| runtime.debug_port);
    let watcher_state = Arc::clone(state);
    tokio::spawn(async move {
        let initial_events = pending_approval::recent_session_events(&codex_home());
        let mut turn_tracker = WebhookTurnTracker::from_snapshot(&initial_events);
        let mut last_synced_thread_statuses = None;
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                _ = interval.tick() => {
                    let events = pending_approval::recent_session_events(&codex_home());
                    if let Some(debug_port) = debug_port
                        && last_synced_thread_statuses.as_ref() != Some(&events.session_statuses)
                    {
                        match cdp::sync_thread_statuses(debug_port, &events.session_statuses).await {
                            Ok(()) => {
                                last_synced_thread_statuses =
                                    Some(events.session_statuses.clone());
                            }
                            Err(error) => {
                                eprintln!("Codey 侧边栏任务状态同步失败：{error:#}");
                            }
                        }
                    }
                    for pending in &events.pending_approvals {
                        let payload = json!({
                            "sessionId": pending.session_id,
                            "turnId": pending.turn_id,
                            "waitingId": pending.waiting_id,
                            "durationMs": pending.duration_ms,
                            "model": "",
                            "reasoningEffort": "",
                        });
                        if let Err(error) = notify_webhook_waiting(&watcher_state, &payload).await {
                            eprintln!("Codey 飞书等待通知失败：{error}");
                        }
                    }
                    for completed in turn_tracker.completion_candidates(&events) {
                        let payload = json!({
                            "sessionId": completed.session_id,
                            "turnId": completed.turn_id,
                            "durationMs": completed.duration_ms,
                            "rolloutError": completed.error,
                            "confirmedByRollout": true,
                        });
                        match notify_webhook_completion(&watcher_state, &payload).await {
                            Ok(_) => turn_tracker.mark_settled(&completed),
                            Err(error) => {
                                // Keep the running edge so a transient delivery failure is retried.
                                eprintln!("Codey 飞书完成通知失败：{error}");
                            }
                        }
                    }
                }
            }
        }
    });
}

async fn stop_waiting_webhook_watcher(state: &Arc<AppState>) {
    if let Some(shutdown) = state.waiting_watcher_shutdown.lock().await.take() {
        let _ = shutdown.send(());
    }
}

pub async fn test_webhook(state: &Arc<AppState>) -> Result<Value, String> {
    let config = state.config.read().await;
    let dispatcher =
        WebhookDispatcher::new(config.webhook.clone()).map_err(|error| error.to_string())?;
    dispatcher.test().await.map_err(|error| error.to_string())
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
    let dispatcher = WebhookDispatcher::new(webhook).map_err(|error| error.to_string())?;
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
    let (model, reasoning_effort) = webhook_session_configuration("", "");
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
    let dispatcher = WebhookDispatcher::new(webhook).map_err(|error| error.to_string())?;
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
    let dispatcher = WebhookDispatcher::new(webhook).map_err(|error| error.to_string())?;
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
    let result = delete_messages(&codex_home(), &session_id, &message_ids)
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
    let result = session_delete::delete_session(&codex_home(), &backup_root, &session_id, &title)
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
    let mut status =
        plugin_marketplace::ensure_marketplaces(&home).map_err(|error| error.to_string())?;
    if let Some(object) = status.as_object_mut() {
        object.insert("status".into(), Value::String("ready".into()));
        object.insert(
            "localMarketplacePath".into(),
            Value::String(home.join(".tmp/plugins").to_string_lossy().to_string()),
        );
    }
    Ok(status)
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
    use crate::pending_approval::{AbortedTurn, StartedTurn};

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
