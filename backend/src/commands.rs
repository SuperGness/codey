use std::collections::HashMap;
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

mod models;
mod runtime;
mod updates;
mod webhooks;

#[cfg(windows)]
use codey_runtime_core::app_paths::resolve_codex_app_dir_with_saved;
use codey_runtime_core::app_paths::{build_codex_executable, normalize_codex_app_path};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, RwLock, oneshot, watch};

#[cfg(test)]
use models::{
    config_with_current_provider_models, preserve_selected_third_party_models,
    preserve_selected_third_party_models_except, renderer_model_catalog_value,
    should_refresh_model_catalog, startup_model_sync_models_or_fallback, sync_cc_switch_state_with,
    validate_deleted_third_party_models, validate_manual_model_selection,
};
use models::{
    current_model_state, current_renderer_model_catalog, sync_provider_models_for_launch,
};
pub use models::{
    fetch_current_provider_models, save_default_model, save_selected_models, sync_cc_switch_state,
    sync_current_provider_command, sync_official_experimental_features,
};
use runtime::refresh_injection_status;
#[cfg(test)]
use runtime::{begin_shutdown, launch_codey_inner};
pub use runtime::{
    launch_codey_runtime, runtime_status, schedule_restart_codey_runtime, stop_codey_runtime,
};
use updates::current_update_platform;
#[cfg(test)]
use updates::{UpdateManifest, assess_update_manifest, current_update_arch};
pub use updates::{check_for_updates, download_update, install_downloaded_update};
use webhooks::{
    WaitingLedgerState, WebhookNotificationState, initial_waiting_notifications,
    sync_waiting_webhook_watcher, test_notification_channel, test_webhook,
};

use crate::account_usage;
use crate::cc_switch;
use crate::cdp;
use crate::codex_config::{codex_home, mark_runtime_subagent_defaults_applied};
use crate::config::{CodeyConfig, ConfigStore};
use crate::error_log;
#[cfg(windows)]
use crate::launcher::{CODEX_APP_NOT_FOUND_ERROR, CODEX_APP_PATH_INVALID_ERROR};
use crate::launcher::{CodeyRuntime, RuntimeModelConfig, RuntimeSubagentConfig};
use crate::message_delete::delete_messages;
#[cfg(test)]
use crate::model_catalog;
use crate::notifications::NotificationChannelConfig;
use crate::pending_approval;
use crate::plugin_marketplace;
use crate::session_delete;
use crate::session_metadata;
use crate::session_transfer;
use crate::trace_log_guard;
use crate::trace_log_stats::{self, TraceLogStatsHandle, TraceLogStatsSnapshot};

const STARTUP_PROVIDER_MODEL_SYNC_TIMEOUT: Duration = Duration::from_secs(5);

pub struct AppState {
    pub store: ConfigStore,
    pub config: RwLock<CodeyConfig>,
    config_write_lock: Mutex<()>,
    pub http_client: reqwest::Client,
    pub webhook_http_client: reqwest::Client,
    pub runtime: Mutex<Option<Arc<CodeyRuntime>>>,
    runtime_operation: Mutex<()>,
    pub trace_log_stats: TraceLogStatsHandle,
    pub startup_error: RwLock<Option<String>>,
    restart_in_progress: AtomicBool,
    shutting_down: AtomicBool,
    restart_task: Mutex<Option<ScheduledRestart>>,
    runtime_generation: AtomicU64,
    session_titles: RwLock<HashMap<String, String>>,
    session_metadata_cache: Mutex<Option<session_metadata::SessionMetadataCache>>,
    webhook_notifications: Mutex<WebhookNotificationState>,
    persisted_waiting_notifications: Mutex<WaitingLedgerState>,
    recent_session_event_cache: Mutex<Option<pending_approval::RecentSessionEventCache>>,
    waiting_watcher_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    waiting_watcher_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    waiting_watcher_sync: Mutex<()>,
    session_scan_wake: Notify,
    restart_settled: Notify,
    shutdown_reason: watch::Sender<Option<AppShutdownReason>>,
}

struct ScheduledRestart {
    cancel: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

struct RestartInProgressGuard {
    state: Arc<AppState>,
}

impl Drop for RestartInProgressGuard {
    fn drop(&mut self) {
        self.state
            .restart_in_progress
            .store(false, Ordering::Release);
        self.state.restart_settled.notify_waiters();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppShutdownReason {
    CodexExited,
    InstallUpdate,
}

impl Default for AppState {
    fn default() -> Self {
        let store = ConfigStore::default();
        let config = store.load().unwrap_or_default();
        let persisted_waiting_notifications = initial_waiting_notifications(&store, &[]);
        let (shutdown_reason, _) = watch::channel(None);
        Self {
            store,
            config: RwLock::new(config),
            config_write_lock: Mutex::new(()),
            http_client: reqwest::Client::builder()
                .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("shared Codey HTTP client should be constructible"),
            webhook_http_client: crate::notifications::notification_http_client()
                .expect("notification HTTP client should be constructible"),
            runtime: Mutex::new(None),
            runtime_operation: Mutex::new(()),
            trace_log_stats: TraceLogStatsHandle::idle(),
            startup_error: RwLock::new(None),
            restart_in_progress: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            restart_task: Mutex::new(None),
            runtime_generation: AtomicU64::new(0),
            session_titles: RwLock::new(HashMap::new()),
            session_metadata_cache: Mutex::new(Some(
                session_metadata::SessionMetadataCache::default(),
            )),
            webhook_notifications: Mutex::new(WebhookNotificationState::from_settled(
                persisted_waiting_notifications.iter().cloned(),
            )),
            persisted_waiting_notifications: Mutex::new(persisted_waiting_notifications),
            recent_session_event_cache: Mutex::new(Some(
                pending_approval::RecentSessionEventCache::default(),
            )),
            waiting_watcher_shutdown: Mutex::new(None),
            waiting_watcher_task: Mutex::new(None),
            waiting_watcher_sync: Mutex::new(()),
            session_scan_wake: Notify::new(),
            restart_settled: Notify::new(),
            shutdown_reason,
        }
    }
}

fn bridge_string(payload: &Value, name: &str) -> String {
    payload
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn bridge_u64(payload: &Value, name: &str) -> Option<u64> {
    payload.get(name).and_then(Value::as_u64)
}

impl AppState {
    pub fn request_shutdown(&self) {
        self.request_shutdown_with_reason(AppShutdownReason::CodexExited);
    }

    pub fn request_update_shutdown(&self) {
        self.request_shutdown_with_reason(AppShutdownReason::InstallUpdate);
    }

    fn request_shutdown_with_reason(&self, reason: AppShutdownReason) {
        self.shutting_down.store(true, Ordering::Release);
        self.shutdown_reason.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reason);
            true
        });
    }

    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    pub async fn wait_for_shutdown(&self) -> AppShutdownReason {
        let mut shutdown_reason = self.shutdown_reason.subscribe();
        loop {
            if let Some(reason) = *shutdown_reason.borrow_and_update() {
                return reason;
            }
            if shutdown_reason.changed().await.is_err() {
                return AppShutdownReason::CodexExited;
            }
        }
    }

    pub async fn bridge_request(self: &Arc<Self>, path: String, payload: Value) -> Value {
        if let Some(command) = path.strip_prefix("/api/") {
            return invoke_api(self, command, payload).await;
        }
        match path.as_str() {
            "/diagnostics/error" => {
                let recorded = error_log::record_renderer_failure(&payload);
                // 启动卡死自动恢复：渲染进程上报 host shell 启动超时后，
                // 由 Codey 主动 reload 页面一次（全程仅一次），watchdog 会在
                // 数秒内重建桥接；网络抖动等临时故障恢复后主界面即可自愈。
                if payload
                    .get("operation")
                    .and_then(serde_json::Value::as_str)
                    == Some("wait_for_codex_host_shell")
                {
                    if let Some(runtime) = self.runtime.lock().await.as_ref() {
                        runtime.maybe_recover_startup_stall().await;
                    }
                }
                match recorded {
                    Ok(()) => json!({
                        "status": "ok",
                        "message": "Renderer 错误诊断已记录",
                    }),
                    Err(error) => api_error_message(error),
                }
            }
            "/settings/get" => {
                let config = self.config.read().await;
                serde_json::to_value(redacted_config(&config))
                    .unwrap_or_else(|_| json!({"status":"failed"}))
            }
            "/codex-model-catalog" => {
                let config = self.config.read().await.clone();
                current_renderer_model_catalog(&config).unwrap_or_else(api_error_message)
            }
            "/backend/status" => {
                let mut value = runtime_status(self).await.unwrap_or_else(api_error_message);
                if let Some(object) = value.as_object_mut() {
                    object.insert("status".into(), Value::String("ok".into()));
                }
                value
            }
            "/account/usage" => account_usage_snapshot(self).await,
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
                        Some(codey_runtime_core::models::SessionRef {
                            session_id: session_id.to_string(),
                            title: session
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                    })
                    .collect::<Vec<_>>();
                let home = codex_home();
                match with_session_metadata_cache(self, "读取线程排序键", move |cache| {
                    cache.thread_sort_keys(&home, &sessions)
                })
                .await
                {
                    Ok(value) => value,
                    Err(error) => api_error_message(error),
                }
            }
            "/session/delete" => {
                let session_id = bridge_string(&payload, "sessionId");
                let title = bridge_string(&payload, "title");
                delete_session_record(self, session_id, title)
                    .await
                    .unwrap_or_else(api_error_message)
            }
            "/session/export/start" => {
                let session_id = bridge_string(&payload, "sessionId");
                let home = codex_home();
                blocking_value("准备会话导出", move || {
                    session_transfer::start_export_transfer(&home, &session_id)
                })
                .await
            }
            "/session/export/chunk" => {
                let transfer_id = bridge_string(&payload, "transferId");
                let Some(offset) = bridge_u64(&payload, "offset") else {
                    return api_error_message("缺少会话导出分块偏移");
                };
                let home = codex_home();
                blocking_value("读取会话导出分块", move || {
                    session_transfer::read_export_transfer_chunk(&home, &transfer_id, offset)
                })
                .await
            }
            "/session/export/finish" | "/session/export/abort" => {
                let transfer_id = bridge_string(&payload, "transferId");
                let home = codex_home();
                blocking_value("清理会话导出", move || {
                    session_transfer::finish_export_transfer(&home, &transfer_id)?;
                    Ok(json!({"status": "ok"}))
                })
                .await
            }
            "/session/import/start" => {
                let home = codex_home();
                blocking_value("准备会话导入", move || {
                    session_transfer::start_import_transfer(&home)
                })
                .await
            }
            "/session/import/chunk" => {
                let transfer_id = bridge_string(&payload, "transferId");
                let data = bridge_string(&payload, "data");
                let Some(offset) = bridge_u64(&payload, "offset") else {
                    return api_error_message("缺少会话导入分块偏移");
                };
                let home = codex_home();
                blocking_value("写入会话导入分块", move || {
                    session_transfer::append_import_transfer_chunk(
                        &home,
                        &transfer_id,
                        offset,
                        &data,
                    )
                })
                .await
            }
            "/session/import/finish" => {
                let transfer_id = bridge_string(&payload, "transferId");
                let project_path = bridge_string(&payload, "projectPath");
                let home = codex_home();
                blocking_value("完成会话导入", move || {
                    session_transfer::finish_import_transfer(&home, &project_path, &transfer_id)
                })
                .await
            }
            "/session/import/abort" => {
                let transfer_id = bridge_string(&payload, "transferId");
                let home = codex_home();
                blocking_value("清理会话导入", move || {
                    session_transfer::abort_import_transfer(&home, &transfer_id)?;
                    Ok(json!({"status": "ok"}))
                })
                .await
            }
            "/session/delete-messages" => {
                let session_id = bridge_string(&payload, "sessionId");
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
                delete_selected_messages(session_id, message_ids)
                    .await
                    .unwrap_or_else(api_error_message)
            }
            "/plugins/list" => {
                let home = codex_home();
                let plugins_home = home.clone();
                match tokio::task::spawn_blocking(move || {
                    plugin_marketplace::list_plugins(&plugins_home)
                })
                .await
                {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) => {
                        error_log::record_failure(
                            "patch_status_failed",
                            "list_plugins",
                            format!("{error:#}"),
                            json!({
                                "codexHome": home,
                            }),
                        );
                        api_error_message(error.to_string())
                    }
                    Err(error) => {
                        error_log::record_failure(
                            "patch_status_failed",
                            "list_plugins",
                            error.to_string(),
                            json!({
                                "codexHome": home,
                                "taskJoinFailed": true,
                            }),
                        );
                        api_error_message(format!("插件列表任务异常退出：{error}"))
                    }
                }
            }
            _ => json!({"status":"failed","message":format!("未知 Codey 路由：{path}")}),
        }
    }
}

pub fn make_bridge_handler(state: &Arc<AppState>) -> codey_runtime_core::bridge::BridgeHandler {
    let state_ref = Arc::clone(state);
    cdp::bridge_handler(move |path, payload| {
        let state_ref = state_ref.clone();
        async move { state_ref.bridge_request(path, payload).await }
    })
}

async fn with_session_metadata_cache<T, F>(
    state: &Arc<AppState>,
    operation: &'static str,
    task: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut session_metadata::SessionMetadataCache) -> T + Send + 'static,
{
    let mut slot = state.session_metadata_cache.lock().await;
    let mut cache = slot.take().unwrap_or_default();
    match tokio::task::spawn_blocking(move || {
        let value = task(&mut cache);
        (cache, value)
    })
    .await
    {
        Ok((cache, value)) => {
            *slot = Some(cache);
            Ok(value)
        }
        Err(error) => {
            *slot = Some(session_metadata::SessionMetadataCache::default());
            Err(format!("{operation}任务异常退出：{error}"))
        }
    }
}

async fn resolve_session_name_cached(
    state: &Arc<AppState>,
    home: PathBuf,
    session_id: String,
    preferred_title: Option<String>,
) -> Result<String, String> {
    with_session_metadata_cache(state, "读取通知会话名称", move |cache| {
        cache.resolve_session_name_with_preferred(&home, &session_id, preferred_title.as_deref())
    })
    .await
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
        "sync_official_experimental_features" => sync_official_experimental_features(state).await,
        "fetch_current_provider_models" => fetch_current_provider_models(state).await,
        "save_selected_models" => match (
            argument::<Vec<String>>(&args, "officialModels"),
            argument::<Vec<String>>(&args, "thirdPartyModels"),
            optional_argument::<Vec<String>>(&args, "manualThirdPartyModels"),
            optional_argument::<Vec<String>>(&args, "deletedThirdPartyModels"),
        ) {
            (
                Ok(official_models),
                Ok(third_party_models),
                Ok(manual_third_party_models),
                Ok(deleted_third_party_models),
            ) => {
                save_selected_models(
                    state,
                    official_models,
                    third_party_models,
                    manual_third_party_models.unwrap_or_default(),
                    deleted_third_party_models.unwrap_or_default(),
                )
                .await
            }
            (Err(error), _, _, _)
            | (_, Err(error), _, _)
            | (_, _, Err(error), _)
            | (_, _, _, Err(error)) => Err(error),
        },
        "save_default_model" => match string_argument(&args, "model") {
            Ok(model) => save_default_model(state, model).await,
            Err(error) => Err(error),
        },
        "runtime_status" => runtime_status(state).await,
        "refresh_injection_status" => refresh_injection_status(state).await,
        "refresh_trace_log_stats" => refresh_trace_log_stats(state).await,
        "launch_codey" => launch_codey_runtime(state).await,
        "restart_codey" => schedule_restart_codey_runtime(state).await,
        "clear_codex_trace_logs" => clear_codex_trace_logs(state).await,
        "test_webhook" => {
            let channel_id = args
                .get("channelId")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            test_webhook(state, channel_id).await
        }
        "test_notification_channel" => {
            match argument::<NotificationChannelConfig>(&args, "channel") {
                Ok(channel) => test_notification_channel(state, channel).await,
                Err(error) => Err(error),
            }
        }
        "reveal_notification_channel" => match string_argument(&args, "channelId") {
            Ok(channel_id) => reveal_notification_channel(state, channel_id).await,
            Err(error) => Err(error),
        },
        "check_for_updates" => check_for_updates(state).await,
        "download_update" => download_update(state).await,
        "install_downloaded_update" => match string_argument(&args, "filePath") {
            Ok(file_path) => install_downloaded_update(state, file_path).await,
            Err(error) => Err(error),
        },
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

async fn reveal_notification_channel(
    state: &Arc<AppState>,
    channel_id: String,
) -> Result<Value, String> {
    let channel_id = channel_id.trim();
    let channel = state
        .config
        .read()
        .await
        .webhook
        .channels
        .iter()
        .find(|channel| channel.id == channel_id)
        .cloned()
        .ok_or_else(|| "找不到要编辑的通知渠道".to_string())?;
    Ok(json!({"channel": channel}))
}

async fn pick_codex_app_directory() -> Result<Value, String> {
    #[cfg(windows)]
    {
        let selected = select_codex_app_directory().await?;

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

#[cfg(windows)]
async fn select_codex_app_directory() -> Result<Option<PathBuf>, String> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择 Codex 桌面应用安装目录（支持任意磁盘）")
            .pick_folder()
    })
    .await
    .map_err(|error| format!("打开 Codex 目录选择器失败：{error}"))
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

#[cfg(windows)]
async fn ensure_windows_codex_app_path(state: &Arc<AppState>) -> Result<(), String> {
    let configured_app_path = state.config.read().await.codex_app_path.trim().to_string();
    let configured_path =
        (!configured_app_path.is_empty()).then(|| PathBuf::from(configured_app_path.as_str()));
    let resolved = tokio::task::spawn_blocking(move || {
        resolve_codex_app_dir_with_saved(configured_path.as_deref(), None)
    })
    .await
    .map_err(|error| format!("检测 Codex 桌面应用目录的任务异常退出：{error}"))?;
    if resolved.is_some() {
        return Ok(());
    }

    let Some(selected) = select_codex_app_directory().await? else {
        let error = if configured_app_path.is_empty() {
            CODEX_APP_NOT_FOUND_ERROR
        } else {
            CODEX_APP_PATH_INVALID_ERROR
        };
        return Err(format!("{error}；已取消选择安装目录"));
    };
    let app_dir = validate_codex_app_path(&selected.to_string_lossy())?;
    let _config_write_guard = state.config_write_lock.lock().await;
    let mut config = state.config.read().await.clone();
    config.codex_app_path = app_dir.to_string_lossy().to_string();
    config.settings_revision = config.settings_revision.saturating_add(1);
    state
        .store
        .save(&config)
        .map_err(|error| format!("保存 Codex 桌面应用目录失败：{error}"))?;
    *state.config.write().await = config;
    Ok(())
}

async fn set_codex_app_path(state: &Arc<AppState>, path: String) -> Result<Value, String> {
    let app_dir = validate_codex_app_path(&path)?;
    let saved = {
        let _config_write_guard = state.config_write_lock.lock().await;
        let mut config = state.config.read().await.clone();
        config.codex_app_path = app_dir.to_string_lossy().to_string();
        save_codey_config_locked(state, config).await
    }?;
    finish_codey_config_save(state, saved).await
}

pub async fn save_codey_config(
    state: &Arc<AppState>,
    config_input: CodeyConfig,
) -> Result<Value, String> {
    let saved = {
        let _config_write_guard = state.config_write_lock.lock().await;
        save_codey_config_locked(state, config_input).await
    }?;
    finish_codey_config_save(state, saved).await
}

struct SavedCodeyConfig {
    config: CodeyConfig,
    restart_required: bool,
    refresh_subagent_defaults: bool,
}

async fn save_codey_config_locked(
    state: &Arc<AppState>,
    mut config_input: CodeyConfig,
) -> Result<SavedCodeyConfig, String> {
    let previous = state.config.read().await.clone();
    if config_input.settings_revision != previous.settings_revision {
        return Err("Codey 设置已被其他操作更新，请关闭后重新打开设置页面再保存".to_string());
    }
    // Provider records, credentials and model-selection caches are read-only
    // through this general settings endpoint.
    let mut config = previous.clone();
    config_input
        .webhook
        .merge_redacted_secrets(&previous.webhook);
    config_input.webhook.validate()?;
    config.webhook = config_input.webhook;
    config.codex_app_path = config_input.codex_app_path;
    config.user_scripts = config_input.user_scripts;
    config.disable_trace_log_writes = config_input.disable_trace_log_writes;
    config.slim_codex_pet = config_input.slim_codex_pet;
    config.slim_codex_voice = config_input.slim_codex_voice;
    config.gpu_launch_mode = config_input.gpu_launch_mode;
    config.fast_context_tools = config_input.fast_context_tools;
    config.fast_codex_startup = config_input.fast_codex_startup;
    config.subagent_optimization = config_input.subagent_optimization;
    config.subagent_model = config_input.subagent_model;
    config.subagent_reasoning_effort = config_input.subagent_reasoning_effort;
    config.hide_full_access_warning = config_input.hide_full_access_warning;
    config.show_account_usage_in_header = config_input.show_account_usage_in_header;
    config.experimental_features = config_input.experimental_features;
    let mut config = config.normalize();
    let refresh_subagent_defaults = previous.subagent_optimization
        && config.subagent_optimization
        && (previous.subagent_model != config.subagent_model
            || previous.subagent_reasoning_effort != config.subagent_reasoning_effort);
    config.settings_revision = previous.settings_revision.saturating_add(1);
    let restart_required = runtime_config_requires_restart(state, &config).await;
    if config.disable_trace_log_writes != previous.disable_trace_log_writes {
        let home = codex_home();
        let disable_writes = config.disable_trace_log_writes;
        let result =
            tokio::task::spawn_blocking(move || trace_log_guard::configure(&home, disable_writes))
                .await
                .map_err(|error| format!("Trace 日志保护切换任务异常退出：{error}"))
                .and_then(|result| result.map_err(|error| error.to_string()));
        if let Err(error) = result {
            error_log::record_failure(
                "patch_failed",
                "configure_trace_log_guard",
                error.clone(),
                json!({
                    "disabled": disable_writes,
                    "source": "save_codey_config",
                }),
            );
            return Err(error);
        }
    }
    state
        .store
        .save(&config)
        .map_err(|error| error.to_string())?;
    *state.config.write().await = config.clone();
    Ok(SavedCodeyConfig {
        config,
        restart_required,
        refresh_subagent_defaults,
    })
}

async fn finish_codey_config_save(
    state: &Arc<AppState>,
    saved: SavedCodeyConfig,
) -> Result<Value, String> {
    sync_waiting_webhook_watcher(state).await;
    let mut subagent_defaults_hot_reloaded = false;
    let mut subagent_defaults_hot_reload_error = None;
    if saved.refresh_subagent_defaults
        && let Some(result) = hot_reload_runtime_subagent_defaults(state, &saved.config).await
    {
        match result {
            Ok(()) => subagent_defaults_hot_reloaded = true,
            Err(error) => subagent_defaults_hot_reload_error = Some(error),
        }
    }
    let restart_required = if saved.refresh_subagent_defaults {
        runtime_config_requires_restart(state, &saved.config).await
    } else {
        saved.restart_required
    };
    let cc_switch = cc_switch::status_from_config(&saved.config);
    let model_state = current_model_state(&saved.config)?;
    let public_config = redacted_config(&saved.config);
    Ok(json!({
        "status":"ok",
        "config":public_config,
        "ccSwitch":cc_switch,
        "modelState":model_state,
        "restartRequired":restart_required,
        "subagentDefaultsHotReloaded":subagent_defaults_hot_reloaded,
        "subagentDefaultsHotReloadError":subagent_defaults_hot_reload_error,
    }))
}

async fn hot_reload_runtime_subagent_defaults(
    state: &Arc<AppState>,
    config: &CodeyConfig,
) -> Option<Result<(), String>> {
    let runtime = state.runtime.lock().await.clone()?;
    let websocket_url = runtime.renderer_websocket_url().await;
    let result = cdp::refresh_subagent_defaults(
        &websocket_url,
        &config.subagent_model,
        &config.subagent_reasoning_effort,
    )
    .await;
    match result {
        Ok(()) => {
            let home = codex_home();
            let model = config.subagent_model.clone();
            let reasoning_effort = config.subagent_reasoning_effort.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || {
                mark_runtime_subagent_defaults_applied(&home, &model, &reasoning_effort)
            })
            .await
            .map_err(|error| format!("子代理运行时租约更新任务异常退出：{error}"))
            .and_then(|result| result.map_err(|error| format!("{error:#}")))
            {
                error_log::record_failure(
                    "patch_verification_failed",
                    "adopt_subagent_defaults_lease",
                    error.clone(),
                    json!({
                        "model": config.subagent_model,
                        "reasoningEffort": config.subagent_reasoning_effort,
                    }),
                );
                return Some(Err(error));
            }
            runtime.mark_subagent_config_applied(config).await;
            Some(Ok(()))
        }
        Err(error) => {
            let error = format!("{error:#}");
            error_log::record_failure(
                "patch_verification_failed",
                "refresh_subagent_defaults",
                error.clone(),
                json!({
                    "model": config.subagent_model,
                    "reasoningEffort": config.subagent_reasoning_effort,
                    "websocketUrl": websocket_url,
                }),
            );
            Some(Err(error))
        }
    }
}

pub async fn clear_codex_trace_logs(state: &Arc<AppState>) -> Result<Value, String> {
    let home = codex_home();
    let disable_writes = state.config.read().await.disable_trace_log_writes;
    let result = tokio::task::spawn_blocking(move || {
        trace_log_guard::configure(&home, disable_writes)?;
        trace_log_guard::clear(&home)
    })
    .await
    .map_err(|error| format!("Trace 日志库清理任务异常退出：{error}"))
    .and_then(|result| result.map_err(|error| error.to_string()));
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "clear_codex_trace_logs",
                error.clone(),
                json!({
                    "protectionEnabled": disable_writes,
                }),
            );
            return Err(error);
        }
    };
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

fn redacted_config(config: &CodeyConfig) -> CodeyConfig {
    let mut public = config.clone();
    for profile in &mut public.profiles {
        profile.api_key.clear();
    }
    public.webhook.url.clear();
    for channel in &mut public.webhook.channels {
        channel.url_configured = !channel.url.trim().is_empty();
        channel.url.clear();
        channel.bot_token_configured = !channel.bot_token.trim().is_empty();
        channel.bot_token.clear();
    }
    public
}

async fn account_usage_snapshot(state: &Arc<AppState>) -> Value {
    let config = state.config.read().await.clone();
    if !config.show_account_usage_in_header {
        return json!({"status": "disabled"});
    }
    if !cc_switch::status_from_config(&config).provider.official {
        return json!({
            "status": "unavailable",
            "reason": "third_party",
            "message": "顶部额度仅支持官方账号线路",
        });
    }

    match account_usage::fetch_official_account_usage(&state.http_client, &codex_home()).await {
        Ok(snapshot) => {
            let mut value = serde_json::to_value(snapshot).unwrap_or_else(|_| json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("status".into(), Value::String("ok".into()));
            }
            value
        }
        Err(error) => json!({
            "status": "error",
            "message": error.to_string(),
        }),
    }
}

fn config_requires_restart(
    applied: &CodeyConfig,
    applied_models: &RuntimeModelConfig,
    applied_subagent: &RuntimeSubagentConfig,
    current: &CodeyConfig,
) -> bool {
    applied.active_profile() != current.active_profile()
        || applied.codex_app_path != current.codex_app_path
        || applied.user_scripts != current.user_scripts
        || applied.slim_codex_pet != current.slim_codex_pet
        || applied.slim_codex_voice != current.slim_codex_voice
        || applied.gpu_launch_mode != current.gpu_launch_mode
        || applied.fast_context_tools != current.fast_context_tools
        || applied.fast_codex_startup != current.fast_codex_startup
        || applied.subagent_optimization != current.subagent_optimization
        || applied.experimental_features != current.experimental_features
        || applied_models != &RuntimeModelConfig::from_config(current)
        || ((applied.subagent_optimization || current.subagent_optimization)
            && applied_subagent != &RuntimeSubagentConfig::from_config(current))
}

async fn runtime_config_requires_restart(state: &Arc<AppState>, current: &CodeyConfig) -> bool {
    let runtime = state.runtime.lock().await.clone();
    let Some(runtime) = runtime else {
        return false;
    };
    let applied_models = runtime.applied_model_config().await;
    let applied_subagent = runtime.applied_subagent_config().await;
    config_requires_restart(
        &runtime.applied_config,
        &applied_models,
        &applied_subagent,
        current,
    )
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    #[test]
    fn renderer_model_catalog_keeps_supported_models_before_configured_models() {
        let mut config = CodeyConfig::default();
        config.profiles[0].cc_switch_provider_id = Some("cc-switch-provider".into());
        let official_models = [
            ("gpt-5.6-sol", "GPT-5.6-Sol"),
            ("gpt-5.6-terra", "GPT-5.6-Terra"),
            ("gpt-5.6-luna", "GPT-5.6-Luna"),
            ("gpt-5.5", "GPT-5.5"),
            ("gpt-5.4", "GPT-5.4"),
            ("gpt-5.4-mini", "GPT-5.4-Mini"),
            ("gpt-5.3-codex-spark", "GPT-5.3-Codex-Spark"),
        ]
        .into_iter()
        .map(
            |(slug, display_name)| model_catalog::OfficialModelAvailability {
                slug: slug.into(),
                display_name: display_name.into(),
                supported: !matches!(slug, "gpt-5.6-terra" | "gpt-5.4-mini"),
                supported_reasoning_efforts: vec![
                    "low".into(),
                    "medium".into(),
                    "high".into(),
                    "xhigh".into(),
                ],
                default_reasoning_effort: "low".into(),
            },
        )
        .collect::<Vec<_>>();
        let model_state = model_catalog::ModelSelectionState {
            official_model_ids: official_models
                .iter()
                .map(|model| model.slug.clone())
                .collect(),
            official_models,
            third_party_models: vec!["provider-fast-coder".into()],
            manual_third_party_models: vec!["provider-fast-coder".into()],
            upstream_models: vec!["provider-fast-coder".into()],
            default_model: "gpt-5.6-sol".into(),
        };

        let catalog = renderer_model_catalog_value(&config, &model_state);

        assert_eq!(
            catalog["models"],
            json!([
                "gpt-5.6-sol",
                "gpt-5.6-luna",
                "gpt-5.5",
                "gpt-5.4",
                "gpt-5.3-codex-spark",
                "provider-fast-coder"
            ])
        );
        assert_eq!(catalog["default_model"], "gpt-5.6-sol");
        assert_eq!(catalog["model_provider"], "cc-switch-provider");
    }

    #[test]
    fn restart_sensitive_config_changes_are_detected() {
        let applied = CodeyConfig::default();
        let applied_models = RuntimeModelConfig::from_config(&applied);
        let applied_subagent = RuntimeSubagentConfig::from_config(&applied);

        let mut model_change = applied.clone();
        let provider_id = model_change.current_provider_id().unwrap().to_string();
        model_change
            .selected_models_by_provider
            .insert(provider_id, vec!["third-party-model".into()]);
        assert!(config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &model_change
        ));
        assert!(!config_requires_restart(
            &applied,
            &RuntimeModelConfig::from_config(&model_change),
            &applied_subagent,
            &model_change
        ));

        let mut default_model_change = applied.clone();
        let provider_id = default_model_change
            .current_provider_id()
            .unwrap()
            .to_string();
        default_model_change
            .default_model_by_provider
            .insert(provider_id, "provider-default".into());
        assert!(config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &default_model_change
        ));
        assert!(!config_requires_restart(
            &applied,
            &RuntimeModelConfig::from_config(&default_model_change),
            &applied_subagent,
            &default_model_change
        ));

        let mut startup_change = applied.clone();
        startup_change.slim_codex_voice = !startup_change.slim_codex_voice;
        assert!(config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &startup_change
        ));

        let mut gpu_mode_change = applied.clone();
        gpu_mode_change.gpu_launch_mode = crate::config::GpuLaunchMode::DisableGpuRasterization;
        assert!(config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &gpu_mode_change
        ));

        let mut account_usage_change = applied.clone();
        account_usage_change.show_account_usage_in_header = true;
        assert!(!config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &account_usage_change
        ));

        let mut fast_startup_change = applied.clone();
        fast_startup_change.fast_codex_startup = !fast_startup_change.fast_codex_startup;
        assert!(config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &fast_startup_change
        ));

        let mut experimental_feature_change = applied.clone();
        experimental_feature_change
            .experimental_features
            .unified_exec = !experimental_feature_change
            .experimental_features
            .unified_exec;
        assert!(config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &experimental_feature_change
        ));

        let mut disabled_subagent_change = applied.clone();
        disabled_subagent_change.subagent_model = "gpt-5.6-sol".into();
        assert!(!config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &disabled_subagent_change
        ));

        let mut enabled_subagents = applied.clone();
        enabled_subagents.subagent_optimization = true;
        let enabled_models = RuntimeModelConfig::from_config(&enabled_subagents);
        let enabled_subagent = RuntimeSubagentConfig::from_config(&enabled_subagents);
        let mut changed_subagent = enabled_subagents.clone();
        changed_subagent.subagent_model = "gpt-5.6-sol".into();
        changed_subagent.subagent_reasoning_effort = "high".into();
        assert!(config_requires_restart(
            &enabled_subagents,
            &enabled_models,
            &enabled_subagent,
            &changed_subagent
        ));
        assert!(!config_requires_restart(
            &enabled_subagents,
            &enabled_models,
            &RuntimeSubagentConfig::from_config(&changed_subagent),
            &changed_subagent
        ));
    }

    #[tokio::test]
    async fn shutdown_cancels_a_restart_waiting_for_the_runtime_lock() {
        let state = Arc::new(AppState::default());
        let _operation = state.runtime_operation.lock().await;
        let response = schedule_restart_codey_runtime(&state).await.unwrap();
        assert_eq!(response["status"], "restarting");
        tokio::time::sleep(Duration::from_millis(275)).await;

        tokio::time::timeout(Duration::from_secs(1), begin_shutdown(&state))
            .await
            .expect("shutdown waited on a restart blocked by the runtime lock");

        assert!(state.is_shutting_down());
        assert!(!state.restart_in_progress.load(Ordering::Acquire));
        assert!(state.restart_task.lock().await.is_none());
    }

    #[tokio::test]
    async fn shutdown_rejects_new_runtime_launches_and_restarts() {
        let state = Arc::new(AppState::default());
        begin_shutdown(&state).await;

        assert!(
            launch_codey_inner(&state)
                .await
                .unwrap_err()
                .contains("正在退出")
        );
        assert!(
            schedule_restart_codey_runtime(&state)
                .await
                .unwrap_err()
                .contains("正在退出")
        );
    }

    #[test]
    fn live_config_changes_do_not_require_restart() {
        let applied = CodeyConfig::default();
        let applied_models = RuntimeModelConfig::from_config(&applied);
        let applied_subagent = RuntimeSubagentConfig::from_config(&applied);
        let mut current = applied.clone();
        current.webhook.channels.push(NotificationChannelConfig {
            url: "https://example.test/webhook".into(),
            ..NotificationChannelConfig::default()
        });
        current.disable_trace_log_writes = !current.disable_trace_log_writes;

        assert!(!config_requires_restart(
            &applied,
            &applied_models,
            &applied_subagent,
            &current
        ));
    }

    #[tokio::test]
    async fn runtime_status_does_not_wait_for_a_lifecycle_operation() {
        let state = Arc::new(AppState::default());
        let _operation = state.runtime_operation.lock().await;

        let status = tokio::time::timeout(Duration::from_millis(100), runtime_status(&state))
            .await
            .expect("runtime status should not wait for the lifecycle operation lock")
            .unwrap();

        assert_eq!(status["running"], false);
    }

    #[test]
    fn successful_startup_model_sync_filters_unsupported_models() {
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config.selected_models_by_provider.insert(
            provider_id,
            vec!["provider-fast-coder".into(), "provider-missing".into()],
        );
        let synced = config_with_current_provider_models(
            &config,
            vec!["gpt-5.6-sol".into(), "provider-fast-coder".into()],
        );
        let home = tempfile::tempdir().unwrap();

        let state = model_catalog::selection_state(
            home.path(),
            false,
            synced.upstream_models_snapshot(),
            synced.selected_models(),
            synced.default_model(),
        )
        .unwrap();

        assert_eq!(
            state
                .official_models
                .iter()
                .filter(|model| model.supported)
                .map(|model| model.slug.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.6-sol"]
        );
        assert_eq!(state.third_party_models, ["provider-fast-coder"]);
    }

    #[test]
    fn failed_startup_model_sync_falls_back_to_exactly_seven_models() {
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config
            .selected_models_by_provider
            .insert(provider_id.clone(), vec!["provider-fast-coder".into()]);
        config
            .default_model_by_provider
            .insert(provider_id, "provider-fast-coder".into());
        let expected = model_catalog::default_official_model_slugs();
        let (fallback_models, synced) = startup_model_sync_models_or_fallback(Vec::new(), None);
        assert!(!synced);
        assert_eq!(fallback_models, expected);
        let fallback = config_with_current_provider_models(&config, fallback_models);
        let home = tempfile::tempdir().unwrap();

        let state = model_catalog::selection_state(
            home.path(),
            false,
            fallback.upstream_models_snapshot(),
            fallback.selected_models(),
            fallback.default_model(),
        )
        .unwrap();

        assert_eq!(
            state
                .official_models
                .iter()
                .filter(|model| model.supported)
                .map(|model| model.slug.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(state.third_party_models.is_empty());
        assert_eq!(state.default_model, "gpt-5.6-sol");
    }

    #[test]
    fn failed_startup_model_sync_preserves_a_saved_manual_selection() {
        let saved = vec!["gpt-5.6-luna".into(), "provider-manual-model".into()];

        let (fallback_models, synced) =
            startup_model_sync_models_or_fallback(Vec::new(), Some(&saved));

        assert!(!synced);
        assert_eq!(fallback_models, saved);
    }

    #[test]
    fn successful_model_sync_preserves_user_confirmed_other_models() {
        let merged = preserve_selected_third_party_models(
            vec!["gpt-5.6-sol".into(), "provider-listed".into()],
            &[
                "provider-manual".into(),
                "provider-listed".into(),
                "gpt-5.4".into(),
            ],
        );

        assert_eq!(
            merged,
            ["gpt-5.6-sol", "provider-listed", "provider-manual",]
        );
    }

    #[test]
    fn manual_model_selection_deletion_removes_saved_other_model_support() {
        let official = model_catalog::default_official_model_slugs();
        let deleted =
            validate_deleted_third_party_models(&official, &["provider-manual".into()]).unwrap();
        let mut supported_models = vec!["gpt-5.6-sol".into()];

        preserve_selected_third_party_models_except(
            &mut supported_models,
            &[
                "provider-listed".into(),
                "provider-manual".into(),
                "gpt-5.4".into(),
            ],
            &deleted,
        );
        preserve_selected_third_party_models_except(
            &mut supported_models,
            &["provider-listed".into()],
            &std::collections::HashSet::new(),
        );

        assert_eq!(supported_models, ["gpt-5.6-sol", "provider-listed"]);
    }

    #[test]
    fn manual_model_selection_deletion_rejects_official_models() {
        let official = model_catalog::default_official_model_slugs();

        let error =
            validate_deleted_third_party_models(&official, &[" GPT-5.6-SOL ".into()]).unwrap_err();

        assert!(error.contains("官方模型"));
    }

    #[test]
    fn manual_model_selection_separates_official_and_other_models() {
        let official = model_catalog::default_official_model_slugs();

        let (supported_official, selected_third_party) = validate_manual_model_selection(
            &official,
            &["gpt-5.6-luna".into(), "gpt-5.4".into()],
            &[
                " provider-manual-model ".into(),
                "provider-manual-model".into(),
            ],
        )
        .unwrap();

        assert_eq!(supported_official, ["gpt-5.6-luna", "gpt-5.4"]);
        assert_eq!(selected_third_party, ["provider-manual-model"]);
    }

    #[test]
    fn manual_model_selection_rejects_official_models_in_the_other_model_input() {
        let official = model_catalog::default_official_model_slugs();

        let error =
            validate_manual_model_selection(&official, &[], &[" GPT-5.6-SOL ".into()]).unwrap_err();

        assert!(error.contains("已在官方模型列表中"));
    }
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

pub async fn delete_selected_messages(
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
    let home = codex_home();
    let result = tokio::task::spawn_blocking(move || {
        session_delete::delete_session(&home, &session_id, &title)
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
    let result = tokio::task::spawn_blocking(move || {
        plugin_marketplace::marketplaces_status(&marketplace_home)
    })
    .await
    .map_err(|error| format!("插件市场状态任务异常退出：{error}"));
    let mut status = match result {
        Ok(status) => status,
        Err(error) => {
            error_log::record_failure(
                "patch_status_failed",
                "read_plugin_marketplace_status",
                error.clone(),
                json!({
                    "codexHome": home,
                }),
            );
            return Err(error);
        }
    };
    decorate_plugin_marketplace_status(&home, &mut status);
    Ok(status)
}

pub async fn repair_plugin_marketplace() -> Result<Value, String> {
    let home = codex_home();
    let marketplace_home = home.clone();
    let result = tokio::task::spawn_blocking(move || {
        plugin_marketplace::ensure_marketplaces(&marketplace_home)
    })
    .await
    .map_err(|error| format!("插件市场修复任务异常退出：{error}"))
    .and_then(|result| result.map_err(|error| error.to_string()));
    let repair = match result {
        Ok(repair) => repair,
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "repair_plugin_marketplace",
                error.clone(),
                json!({
                    "codexHome": home,
                }),
            );
            return Err(error);
        }
    };
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

fn optional_argument<T: DeserializeOwned>(args: &Value, name: &str) -> Result<Option<T>, String> {
    args.get(name)
        .cloned()
        .map(serde_json::from_value)
        .transpose()
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

async fn blocking_value<T, F>(operation: &str, task: F) -> Value
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let operation = operation.to_string();
    let task_operation = operation.clone();
    match tokio::task::spawn_blocking(move || {
        task().and_then(|result| {
            serde_json::to_value(result)
                .map_err(|error| anyhow::anyhow!("{task_operation}结果序列化失败：{error}"))
        })
    })
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => api_error_message(error),
        Err(error) => api_error_message(format!("{operation}任务异常退出：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_field_helpers_preserve_existing_payload_semantics() {
        let payload = json!({
            "text": "  value  ",
            "offset": 42,
            "wrongText": 7,
            "wrongOffset": "42",
        });

        assert_eq!(bridge_string(&payload, "text"), "  value  ");
        assert_eq!(bridge_string(&payload, "missing"), "");
        assert_eq!(bridge_string(&payload, "wrongText"), "");
        assert_eq!(bridge_u64(&payload, "offset"), Some(42));
        assert_eq!(bridge_u64(&payload, "missing"), None);
        assert_eq!(bridge_u64(&payload, "wrongOffset"), None);
    }

    #[test]
    fn renderer_settings_clear_provider_and_notification_secrets() {
        let mut config = CodeyConfig::default();
        config.profiles[0].api_key = "renderer-secret".to_string();
        config.hide_full_access_warning = true;
        config.webhook.url = "https://open.feishu.cn/legacy-secret".to_string();
        config.webhook.channels.push(NotificationChannelConfig {
            id: "feishu-1".to_string(),
            url: "https://open.feishu.cn/open-apis/bot/v2/hook/renderer-secret".to_string(),
            ..NotificationChannelConfig::default()
        });
        config.webhook.channels.push(NotificationChannelConfig {
            id: "telegram-1".to_string(),
            kind: crate::notifications::NotificationChannelKind::Telegram,
            bot_token: "telegram-secret".to_string(),
            chat_id: "-100123".to_string(),
            ..NotificationChannelConfig::default()
        });

        let public = serde_json::to_value(redacted_config(&config)).unwrap();

        assert_eq!(public["profiles"][0]["apiKey"], "");
        assert_eq!(public["hideFullAccessWarning"], true);
        assert!(public["webhook"].get("url").is_none());
        assert_eq!(public["webhook"]["channels"][0]["url"], "");
        assert_eq!(public["webhook"]["channels"][0]["urlConfigured"], true);
        assert_eq!(public["webhook"]["channels"][1]["botToken"], "");
        assert_eq!(public["webhook"]["channels"][1]["botTokenConfigured"], true);
        assert!(!public.to_string().contains("renderer-secret"));
        assert!(!public.to_string().contains("telegram-secret"));
        assert!(!public.to_string().contains("legacy-secret"));
    }

    #[tokio::test]
    async fn settings_bridge_matches_the_redacted_config_contract() {
        let state = Arc::new(AppState::default());
        let mut config = state.config.read().await.clone();
        config.profiles[0].api_key = "bridge-provider-secret".to_string();
        config.webhook.channels.push(NotificationChannelConfig {
            id: "bridge-feishu".to_string(),
            url: "https://open.feishu.cn/open-apis/bot/v2/hook/bridge-secret".to_string(),
            ..NotificationChannelConfig::default()
        });
        let expected = serde_json::to_value(redacted_config(&config)).unwrap();
        *state.config.write().await = config;

        let actual = state
            .bridge_request("/settings/get".to_string(), json!({}))
            .await;

        assert_eq!(actual, expected);
        assert!(!actual.to_string().contains("bridge-provider-secret"));
        assert!(!actual.to_string().contains("bridge-secret"));
    }

    #[tokio::test]
    async fn explicit_notification_channel_reveal_returns_only_the_selected_channel() {
        let state = Arc::new(AppState::default());
        state.config.write().await.webhook.channels.extend([
            NotificationChannelConfig {
                id: "feishu-1".to_string(),
                url: "https://open.feishu.cn/open-apis/bot/v2/hook/reveal-secret".to_string(),
                ..NotificationChannelConfig::default()
            },
            NotificationChannelConfig {
                id: "telegram-1".to_string(),
                kind: crate::notifications::NotificationChannelKind::Telegram,
                bot_token: "telegram-reveal-secret".to_string(),
                chat_id: "-100123".to_string(),
                ..NotificationChannelConfig::default()
            },
        ]);

        let revealed = reveal_notification_channel(&state, "telegram-1".to_string())
            .await
            .unwrap();

        assert_eq!(revealed["channel"]["id"], "telegram-1");
        assert_eq!(revealed["channel"]["botToken"], "telegram-reveal-secret");
        assert!(!revealed.to_string().contains("hook/reveal-secret"));
        assert!(
            reveal_notification_channel(&state, "unknown".to_string())
                .await
                .unwrap_err()
                .contains("找不到")
        );
    }

    #[tokio::test]
    async fn testing_an_incomplete_notification_draft_does_not_save_it() {
        let state = Arc::new(AppState::default());
        let before = state.config.read().await.clone();

        let result = invoke_api(
            &state,
            "test_notification_channel",
            json!({
                "channel": {
                    "id": "incomplete-telegram",
                    "kind": "telegram",
                    "enabled": true,
                    "botToken": "",
                    "chatId": ""
                }
            }),
        )
        .await;

        assert_eq!(result["status"], "failed");
        assert_eq!(*state.config.read().await, before);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stale_concurrent_config_saves_are_rejected_without_diverging_disk_and_memory() {
        let directory = tempfile::tempdir().unwrap();
        let initial = CodeyConfig::default();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(initial.clone()),
            ..AppState::default()
        });
        let save_count = 8;
        let barrier = Arc::new(tokio::sync::Barrier::new(save_count + 1));
        let tasks = (0..save_count)
            .map(|index| {
                let state = Arc::clone(&state);
                let barrier = Arc::clone(&barrier);
                let mut input = initial.clone();
                input.user_scripts = vec![format!("// concurrent save {index}")];
                tokio::spawn(async move {
                    barrier.wait().await;
                    save_codey_config(&state, input).await
                })
            })
            .collect::<Vec<_>>();

        barrier.wait().await;
        let mut successes = 0;
        let mut conflicts = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => successes += 1,
                Err(error) => {
                    assert!(error.contains("已被其他操作更新"));
                    conflicts += 1;
                }
            }
        }

        assert_eq!(successes, 1);
        assert_eq!(conflicts, save_count - 1);
        let memory = state.config.read().await.clone();
        let disk = state.store.load().unwrap();
        assert_eq!(disk, memory);
        assert_eq!(memory.settings_revision, 1);
        assert_eq!(memory.user_scripts.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watcher_join_does_not_hold_the_config_write_lock() {
        let directory = tempfile::tempdir().unwrap();
        let initial = CodeyConfig::default();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(initial.clone()),
            ..AppState::default()
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (shutdown_seen_tx, shutdown_seen_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let watcher_release = Arc::clone(&release);
        let watcher_task = tokio::spawn(async move {
            let _ = shutdown_rx.await;
            let _ = shutdown_seen_tx.send(());
            watcher_release.notified().await;
        });
        *state.waiting_watcher_shutdown.lock().await = Some(shutdown_tx);
        *state.waiting_watcher_task.lock().await = Some(watcher_task);

        let mut input = initial;
        input.slim_codex_pet = !input.slim_codex_pet;
        let save_state = Arc::clone(&state);
        let save_task = tokio::spawn(async move { save_codey_config(&save_state, input).await });
        tokio::time::timeout(Duration::from_secs(1), shutdown_seen_rx)
            .await
            .expect("watcher shutdown should start")
            .unwrap();

        let config_guard =
            tokio::time::timeout(Duration::from_millis(100), state.config_write_lock.lock())
                .await
                .expect("watcher join must happen after releasing the config write lock");
        drop(config_guard);
        release.notify_one();
        save_task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_sync_does_not_block_config_writes_or_commit_a_stale_result() {
        let directory = tempfile::tempdir().unwrap();
        let initial = CodeyConfig::default();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(initial.clone()),
            ..AppState::default()
        });
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let sync_state = Arc::clone(&state);
        let sync_task = tokio::spawn(async move {
            sync_cc_switch_state_with(&sync_state, move |mut config| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                config.profiles[0].name = "stale provider".to_string();
                let mut status = cc_switch::status_from_config(&config);
                status.changed = true;
                Ok((config, status))
            })
            .await
        });
        started_rx.await.unwrap();

        let mut settings = initial;
        settings.slim_codex_pet = !settings.slim_codex_pet;
        tokio::time::timeout(
            Duration::from_millis(500),
            save_codey_config(&state, settings),
        )
        .await
        .expect("provider inspection must not hold the config write lock")
        .unwrap();
        release_tx.send(()).unwrap();

        let status = sync_task.await.unwrap();
        assert!(
            status
                .message
                .as_deref()
                .is_some_and(|message| message.contains("已忽略过期"))
        );
        let memory = state.config.read().await.clone();
        let disk = state.store.load().unwrap();
        assert_eq!(disk, memory);
        assert_ne!(memory.profiles[0].name, "stale provider");
        assert_eq!(memory.settings_revision, 1);
    }

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
    fn selected_codex_app_path_accepts_a_custom_install_root() {
        let directory = tempfile::tempdir().unwrap();
        let install_root = directory.path().join("D drive").join("OpenAI Codex");
        let current = install_root.join("versions").join("current");
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("ChatGPT.exe"), []).unwrap();

        assert_eq!(
            validate_codex_app_path(install_root.to_str().unwrap()).unwrap(),
            current
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
    fn update_manifest_selects_only_a_supported_current_platform_installer() {
        let platform = current_update_platform();
        let arch = current_update_arch();
        let (package_type, file_name, expected_package_type) = match platform {
            "windows" => (
                "nsis",
                format!("Codey-0.2.0-windows-{arch}-setup.exe"),
                Some("nsis"),
            ),
            "macos" => (
                "app-zip",
                format!("Codey-0.2.0-macos-{arch}-unsigned.zip"),
                Some("app-zip"),
            ),
            _ => (
                "app-zip",
                format!("Codey-0.2.0-{platform}-{arch}-unsupported.zip"),
                None,
            ),
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
                    "file_name": &file_name,
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
            expected_package_type
        );
        assert_eq!(
            result
                .selected_asset
                .as_ref()
                .map(|asset| asset.arch.as_str()),
            expected_package_type.map(|_| arch)
        );
        assert_eq!(
            result
                .selected_asset
                .as_ref()
                .map(|asset| asset.file_name.as_str()),
            expected_package_type.map(|_| file_name.as_str())
        );
    }

    #[tokio::test]
    async fn app_state_preserves_update_shutdown_reason() {
        let state = AppState::default();

        state.request_update_shutdown();
        state.request_shutdown();

        assert_eq!(
            state.wait_for_shutdown().await,
            AppShutdownReason::InstallUpdate
        );
    }

    #[tokio::test]
    async fn shutdown_signal_wakes_every_waiter_without_losing_the_reason() {
        let state = Arc::new(AppState::default());
        let waiters = (0..8)
            .map(|_| {
                let state = state.clone();
                tokio::spawn(async move { state.wait_for_shutdown().await })
            })
            .collect::<Vec<_>>();
        tokio::task::yield_now().await;

        state.request_update_shutdown();

        for waiter in waiters {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), waiter)
                    .await
                    .expect("shutdown waiter timed out")
                    .expect("shutdown waiter panicked"),
                AppShutdownReason::InstallUpdate
            );
        }
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
}
