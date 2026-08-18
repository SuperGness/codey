use std::collections::HashMap;
#[cfg(all(test, windows))]
use std::fs;
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex as BlockingMutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

mod diagnostics;
mod models;
mod plugins;
mod prompt_optimization;
mod runtime;
mod updates;
mod webhooks;

#[cfg(windows)]
use codey_runtime_core::app_paths::{
    build_codex_executable, normalize_codex_app_path, resolve_codex_app_dir_with_saved,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, RwLock, oneshot, watch};

use diagnostics::{
    clear_codex_trace_logs, clear_diagnostic_storage, refresh_diagnostic_storage_stats,
    refresh_trace_log_stats,
};
#[cfg(test)]
use models::{
    config_with_current_provider_models, preserve_selected_third_party_models,
    preserve_selected_third_party_models_except, renderer_model_catalog_value,
    should_refresh_model_catalog, startup_model_sync_models_or_fallback, sync_cc_switch_state_with,
    validate_deleted_third_party_models, validate_manual_model_selection,
};
use models::{
    current_model_state_async, current_renderer_model_catalog_async,
    provider_route_requires_restart, sync_provider_models_for_launch,
};
pub use models::{
    fetch_current_provider_models, save_default_model, save_selected_models, sync_cc_switch_state,
    sync_current_provider_command,
};
use plugins::{plugin_marketplace_status, repair_plugin_marketplace};
use prompt_optimization::{
    fetch_prompt_optimization_models_command, optimize_prompt_command,
    sync_prompt_optimization_current_provider_command, test_prompt_optimization_command,
};
pub(crate) use runtime::{
    CC_SWITCH_ROUTE_RECOVERY_INTERVAL, CC_SWITCH_ROUTE_RECOVERY_STABLE_READS,
    cc_switch_route_ready_for_recovery, is_cc_switch_route_recovery_error,
};
#[cfg(test)]
use runtime::{begin_shutdown, launch_codey_inner};
pub use runtime::{
    launch_codey_runtime, runtime_status, schedule_restart_codey_runtime, stop_codey_runtime,
};
use runtime::{refresh_injection_status, runtime_status_with_options};
use updates::current_update_platform;
#[cfg(test)]
pub(crate) use updates::{UpdateAssetInfo, UpdateCheck};
pub(crate) use updates::{
    UpdateCandidate, UpdateDownload, check_for_update_candidate, download_update_candidate,
    start_downloaded_update,
};
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
use crate::codex_config::{
    FastContextToolsStatus, codex_home, fast_context_tools_status, refresh_runtime_subagent_roles,
};
use crate::config::{
    CodeyConfig, ConfigStore, PromptOptimizationConfig, SUBAGENT_ROLE_DEFAULT, SUBAGENT_ROLE_IDS,
    SubagentRoleConfig,
};
use crate::crashpad_pending_guard::{
    self, CrashpadPendingStatsHandle, CrashpadPendingStatsSnapshot,
};
use crate::error_log;
#[cfg(windows)]
use crate::launcher::{CODEX_APP_NOT_FOUND_ERROR, CODEX_APP_PATH_INVALID_ERROR};
use crate::launcher::{CodeyRuntime, RuntimeModelConfig, RuntimeSubagentConfig};
use crate::message_delete::delete_messages_persistently;
#[cfg(test)]
use crate::model_catalog;
use crate::notifications::NotificationChannelConfig;
use crate::pending_approval;
use crate::plugin_marketplace;
use crate::session_delete;
use crate::session_metadata;
use crate::session_transfer;
use crate::subagent_policy;
use crate::token_stats;
use crate::trace_log_guard;
use crate::trace_log_stats::TraceLogStatsHandle;

const STARTUP_PROVIDER_MODEL_SYNC_TIMEOUT: Duration = Duration::from_secs(5);

pub struct AppState {
    pub store: ConfigStore,
    pub config: RwLock<CodeyConfig>,
    config_write_lock: Mutex<()>,
    provider_model_sync_lock: Mutex<()>,
    pub http_client: reqwest::Client,
    pub webhook_http_client: reqwest::Client,
    account_usage_cache: Mutex<account_usage::AccountUsageCache>,
    pub runtime: Mutex<Option<Arc<CodeyRuntime>>>,
    runtime_operation: Mutex<()>,
    diagnostic_storage_operation: Mutex<()>,
    pub trace_log_stats: TraceLogStatsHandle,
    pub crashpad_pending_stats: CrashpadPendingStatsHandle,
    pub startup_error: RwLock<Option<String>>,
    available_update: RwLock<Option<updates::UpdateCheck>>,
    update_candidate_cache: Mutex<Option<updates::CachedUpdateCandidate>>,
    codex_app_version_cache: Mutex<Option<runtime::CodexAppVersionCache>>,
    restart_in_progress: AtomicBool,
    shutting_down: AtomicBool,
    restart_task: Mutex<Option<ScheduledRestart>>,
    runtime_generation: AtomicU64,
    session_titles: RwLock<HashMap<String, String>>,
    session_metadata_cache: BlockingMutex<session_metadata::SessionMetadataCache>,
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
        let protect_crashpad_pending = config.protect_crashpad_pending;
        let persisted_waiting_notifications = initial_waiting_notifications(&store, &[]);
        let (shutdown_reason, _) = watch::channel(None);
        Self {
            store,
            config: RwLock::new(config),
            config_write_lock: Mutex::new(()),
            provider_model_sync_lock: Mutex::new(()),
            http_client: reqwest::Client::builder()
                .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("shared Codey HTTP client should be constructible"),
            webhook_http_client: crate::notifications::notification_http_client()
                .expect("notification HTTP client should be constructible"),
            account_usage_cache: Mutex::new(account_usage::AccountUsageCache::default()),
            runtime: Mutex::new(None),
            runtime_operation: Mutex::new(()),
            diagnostic_storage_operation: Mutex::new(()),
            trace_log_stats: TraceLogStatsHandle::idle(),
            crashpad_pending_stats: CrashpadPendingStatsHandle::idle(protect_crashpad_pending),
            startup_error: RwLock::new(None),
            available_update: RwLock::new(None),
            update_candidate_cache: Mutex::new(None),
            codex_app_version_cache: Mutex::new(None),
            restart_in_progress: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            restart_task: Mutex::new(None),
            runtime_generation: AtomicU64::new(0),
            session_titles: RwLock::new(HashMap::new()),
            session_metadata_cache: BlockingMutex::new(
                session_metadata::SessionMetadataCache::default(),
            ),
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
            "/settings/get" => {
                let config = self.config.read().await;
                serde_json::to_value(redacted_config(&config))
                    .unwrap_or_else(|_| json!({"status":"failed"}))
            }
            "/codex-model-catalog" => {
                let current_config = self.config.read().await.clone();
                let runtime = self.runtime.lock().await.clone();
                let catalog_config = model_catalog_config_for_runtime(
                    &current_config,
                    runtime.as_ref().map(|runtime| &runtime.applied_config),
                )
                .clone();
                current_renderer_model_catalog_async(&catalog_config)
                    .await
                    .unwrap_or_else(api_error_message)
            }
            "/backend/status" => {
                let mut value = runtime_status(self).await.unwrap_or_else(api_error_message);
                if let Some(object) = value.as_object_mut() {
                    object.insert("status".into(), Value::String("ok".into()));
                }
                value
            }
            "/account/usage" => account_usage_snapshot(self).await,
            "/token-stats" => token_stats::token_stats_snapshot(self, &payload).await,
            "/session/wake-watcher" => {
                self.session_scan_wake.notify_one();
                json!({"status":"ok"})
            }
            "/session/titles" => cache_session_titles(self, &payload).await,
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
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let mut cache = state
            .session_metadata_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        task(&mut cache)
    })
    .await
    .map_err(|error| format!("{operation}任务异常退出：{error}"))
}

async fn save_config_to_store(state: &AppState, config: &CodeyConfig) -> Result<(), String> {
    let store = state.store.clone();
    let config = config.clone();
    tokio::task::spawn_blocking(move || store.save(&config))
        .await
        .map_err(|error| format!("保存 Codey 配置任务异常退出：{error}"))?
        .map_err(|error| error.to_string())
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
        "save_codey_config" => match codey_config_save_input(&args) {
            Ok(input) => save_codey_config_input(state, input).await,
            Err(error) => Err(error),
        },
        "sync_current_provider" => sync_current_provider_command(state).await,
        "sync_prompt_optimization_current_provider" => {
            match optional_argument::<PromptOptimizationConfig>(&args, "config") {
                Ok(draft) => sync_prompt_optimization_current_provider_command(state, draft).await,
                Err(error) => Err(error),
            }
        }
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
        "runtime_status" => {
            let refresh_injection_status = args
                .get("refreshInjectionStatus")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            runtime_status_with_options(state, refresh_injection_status).await
        }
        "refresh_injection_status" => refresh_injection_status(state).await,
        "refresh_diagnostic_storage_stats" => refresh_diagnostic_storage_stats(state).await,
        "refresh_trace_log_stats" => refresh_trace_log_stats(state).await,
        "launch_codey" => launch_codey_runtime(state).await,
        "restart_codey" => schedule_restart_codey_runtime(state).await,
        "clear_diagnostic_storage" => clear_diagnostic_storage(state).await,
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
        "reveal_prompt_optimization_api_key" => reveal_prompt_optimization_api_key(state).await,
        "optimize_prompt" => match string_argument(&args, "text") {
            Ok(text) => optimize_prompt_command(state, text).await,
            Err(error) => Err(error),
        },
        "test_prompt_optimization" => {
            match optional_argument::<PromptOptimizationConfig>(&args, "config") {
                Ok(draft) => test_prompt_optimization_command(state, draft).await,
                Err(error) => Err(error),
            }
        }
        "fetch_prompt_optimization_models" => {
            match optional_argument::<PromptOptimizationConfig>(&args, "config") {
                Ok(draft) => fetch_prompt_optimization_models_command(state, draft).await,
                Err(error) => Err(error),
            }
        }
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
    let cc_switch = cc_switch::status_from_config(&config);
    let model_state = current_model_state_async(&config).await?;
    let fast_context_tools_status = current_fast_context_tools_status();
    let mut public_config = redacted_config(&config);
    public_config.fast_context_tools = embedded_fast_context_tools_enabled(
        public_config.fast_context_tools,
        &fast_context_tools_status,
    );
    Ok(json!({
        "config": public_config,
        "path": state.store.path().to_string_lossy(),
        "startupError": startup_error,
        "ccSwitch": cc_switch,
        "modelState": model_state,
        "fastContextToolsStatus": fast_context_tools_status,
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

async fn reveal_prompt_optimization_api_key(state: &Arc<AppState>) -> Result<Value, String> {
    let api_key = state
        .config
        .read()
        .await
        .prompt_optimization
        .api_key
        .clone();
    if api_key.trim().is_empty() {
        return Err("提示词优化 API Key 尚未保存".to_string());
    }
    Ok(json!({"apiKey": api_key}))
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

#[cfg(windows)]
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
    save_config_to_store(state, &config)
        .await
        .map_err(|error| format!("保存 Codex 桌面应用目录失败：{error}"))?;
    *state.config.write().await = config;
    Ok(())
}

#[cfg(test)]
pub async fn save_codey_config(
    state: &Arc<AppState>,
    config_input: CodeyConfig,
) -> Result<Value, String> {
    save_codey_config_input(state, CodeyConfigSaveInput::complete(config_input)).await
}

struct CodeyConfigSaveInput {
    config: CodeyConfig,
    subagent_roles_present: bool,
    subagent_model_present: bool,
    subagent_reasoning_effort_present: bool,
}

#[cfg(test)]
impl CodeyConfigSaveInput {
    fn complete(config: CodeyConfig) -> Self {
        Self {
            config,
            subagent_roles_present: true,
            subagent_model_present: true,
            subagent_reasoning_effort_present: true,
        }
    }
}

fn codey_config_save_input(args: &Value) -> Result<CodeyConfigSaveInput, String> {
    let config_value = args
        .get("config")
        .cloned()
        .ok_or_else(|| "缺少参数：config".to_string())?;
    let fields = config_value
        .as_object()
        .ok_or_else(|| "参数 config 无效：必须是 object".to_string())?;
    let subagent_roles_present = fields.contains_key("subagentRoles");
    let subagent_model_present = fields.contains_key("subagentModel");
    let subagent_reasoning_effort_present = fields.contains_key("subagentReasoningEffort");
    let config = serde_json::from_value(config_value)
        .map_err(|error| format!("参数 config 无效：{error}"))?;
    Ok(CodeyConfigSaveInput {
        config,
        subagent_roles_present,
        subagent_model_present,
        subagent_reasoning_effort_present,
    })
}

async fn save_codey_config_input(
    state: &Arc<AppState>,
    config_input: CodeyConfigSaveInput,
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
    refresh_subagent_config: bool,
    fast_context_tools_status: FastContextToolsStatus,
}

async fn save_codey_config_locked(
    state: &Arc<AppState>,
    input: CodeyConfigSaveInput,
) -> Result<SavedCodeyConfig, String> {
    let CodeyConfigSaveInput {
        config: mut config_input,
        subagent_roles_present,
        subagent_model_present,
        subagent_reasoning_effort_present,
    } = input;
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
    config_input
        .prompt_optimization
        .merge_redacted_secrets(&previous.prompt_optimization);
    config_input.prompt_optimization.validate()?;
    config.prompt_optimization = config_input.prompt_optimization;
    config.codex_app_path = config_input.codex_app_path;
    config.user_scripts = config_input.user_scripts;
    config.disable_trace_log_writes = config_input.disable_trace_log_writes;
    config.protect_crashpad_pending = config_input.protect_crashpad_pending;
    config.slim_codex_pet = config_input.slim_codex_pet;
    config.gpu_launch_mode = config_input.gpu_launch_mode;
    let fast_context_tools_status = current_fast_context_tools_status();
    config.fast_context_tools = embedded_fast_context_tools_enabled(
        config_input.fast_context_tools,
        &fast_context_tools_status,
    );
    config.fast_codex_startup = config_input.fast_codex_startup;
    config.subagent_optimization = config_input.subagent_optimization;
    let default_role_supplied = subagent_roles_present
        && !config_input.subagent_roles.is_empty()
        && config_input
            .subagent_roles
            .contains_key(SUBAGENT_ROLE_DEFAULT);
    if subagent_roles_present && !config_input.subagent_roles.is_empty() {
        for (role, selection) in config_input.subagent_roles {
            if SUBAGENT_ROLE_IDS.contains(&role.as_str()) {
                config.subagent_roles.insert(role, selection);
            }
        }
    }
    if !default_role_supplied && (subagent_model_present || subagent_reasoning_effort_present) {
        let fallback_model = config.subagent_model.clone();
        let fallback_effort = config.subagent_reasoning_effort.clone();
        let default_role = config
            .subagent_roles
            .entry(SUBAGENT_ROLE_DEFAULT.to_string())
            .or_insert_with(|| SubagentRoleConfig::new(fallback_model, fallback_effort));
        if subagent_model_present {
            default_role.model = config_input.subagent_model;
        }
        if subagent_reasoning_effort_present {
            default_role.reasoning_effort = config_input.subagent_reasoning_effort;
        }
    }
    config.hide_full_access_warning = config_input.hide_full_access_warning;
    config.show_account_usage_in_header = config_input.show_account_usage_in_header;
    config.show_token_stats_card = config_input.show_token_stats_card;
    let mut config = config.normalize();
    if config.subagent_optimization
        && let Ok(model_state) = current_model_state_async(&config).await
    {
        subagent_policy::reconcile_with_model_state(&mut config, Some(&model_state));
    }
    // Codex resolves role declarations at startup but reads each registered
    // config_file again when spawning a child. Rebuild the stable runtime files
    // only when an already-enabled policy changed; enabling or disabling the
    // feature itself still requires a restart to register/unregister tools and
    // hooks.
    let refresh_subagent_config = previous.subagent_optimization
        && config.subagent_optimization
        && RuntimeSubagentConfig::from_config(&previous)
            != RuntimeSubagentConfig::from_config(&config);
    config.settings_revision = previous.settings_revision.saturating_add(1);
    let restart_required = runtime_config_requires_restart(state, &config).await;
    let trace_guard_changed = config.disable_trace_log_writes != previous.disable_trace_log_writes;
    let _diagnostic_operation = if trace_guard_changed {
        Some(state.diagnostic_storage_operation.lock().await)
    } else {
        None
    };
    if trace_guard_changed {
        let home = codex_home();
        let disable_writes = config.disable_trace_log_writes;
        let result = configure_trace_log_guard(home.clone(), disable_writes).await;
        if let Err(error) = result {
            let error =
                rollback_trace_log_guard(home, previous.disable_trace_log_writes, error).await;
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
    if let Err(error) = save_config_to_store(state, &config).await {
        if trace_guard_changed {
            return Err(rollback_trace_log_guard(
                codex_home(),
                previous.disable_trace_log_writes,
                error,
            )
            .await);
        }
        return Err(error);
    }
    *state.config.write().await = config.clone();
    Ok(SavedCodeyConfig {
        config,
        restart_required,
        refresh_subagent_config,
        fast_context_tools_status,
    })
}

fn current_fast_context_tools_status() -> FastContextToolsStatus {
    fast_context_tools_status_or_blocked(fast_context_tools_status(&codex_home()))
}

fn fast_context_tools_status_or_blocked<E>(
    status: Result<FastContextToolsStatus, E>,
) -> FastContextToolsStatus {
    status.unwrap_or(FastContextToolsStatus {
        user_configured: false,
        detection_failed: true,
        server_id: None,
    })
}

fn embedded_fast_context_tools_enabled(requested: bool, status: &FastContextToolsStatus) -> bool {
    requested && !status.user_configured && !status.detection_failed
}

async fn configure_trace_log_guard(home: PathBuf, disable_writes: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || trace_log_guard::configure(&home, disable_writes))
        .await
        .map_err(|error| format!("Trace 日志保护切换任务异常退出：{error}"))?
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn rollback_trace_log_guard(
    home: PathBuf,
    previous_disable_writes: bool,
    primary_error: String,
) -> String {
    match configure_trace_log_guard(home, previous_disable_writes).await {
        Ok(()) => primary_error,
        Err(rollback_error) => {
            error_log::record_failure(
                "restore_failed",
                "rollback_trace_log_guard",
                rollback_error.clone(),
                json!({
                    "disabled": previous_disable_writes,
                    "source": "save_codey_config",
                }),
            );
            format!("{primary_error}；回滚 Trace 日志保护也失败：{rollback_error}")
        }
    }
}

async fn finish_codey_config_save(
    state: &Arc<AppState>,
    saved: SavedCodeyConfig,
) -> Result<Value, String> {
    sync_waiting_webhook_watcher(state).await;
    if let Some(runtime) = state.runtime.lock().await.clone() {
        runtime.set_crashpad_pending_protection(saved.config.protect_crashpad_pending);
    }
    schedule_crashpad_pending_refresh(state, saved.config.protect_crashpad_pending);
    let mut subagent_config_hot_reloaded = false;
    let mut subagent_config_hot_reload_error = None;
    if saved.refresh_subagent_config
        && let Some(result) = hot_reload_runtime_subagent_config(state, &saved.config).await
    {
        match result {
            Ok(()) => subagent_config_hot_reloaded = true,
            Err(error) => subagent_config_hot_reload_error = Some(error),
        }
    }
    let restart_required = if saved.refresh_subagent_config {
        runtime_config_requires_restart(state, &saved.config).await
    } else {
        saved.restart_required
    };
    let cc_switch = cc_switch::status_from_config(&saved.config);
    let model_state = current_model_state_async(&saved.config).await?;
    let public_config = redacted_config(&saved.config);
    Ok(json!({
        "status":"ok",
        "config":public_config,
        "ccSwitch":cc_switch,
        "modelState":model_state,
        "fastContextToolsStatus":saved.fast_context_tools_status,
        "restartRequired":restart_required,
        "subagentConfigHotReloaded":subagent_config_hot_reloaded,
        "subagentConfigHotReloadError":subagent_config_hot_reload_error.clone(),
        // Keep the original response keys for older injected consoles.
        "subagentDefaultsHotReloaded":subagent_config_hot_reloaded,
        "subagentDefaultsHotReloadError":subagent_config_hot_reload_error,
    }))
}

fn schedule_crashpad_pending_refresh(state: &Arc<AppState>, protection_enabled: bool) {
    if !state
        .crashpad_pending_stats
        .begin_refresh(protection_enabled)
    {
        return;
    }
    let stats = state.crashpad_pending_stats.clone();
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            if protection_enabled {
                crashpad_pending_guard::enforce_system_limit()
            } else {
                crashpad_pending_guard::CrashpadGuardRun {
                    cleanup: crashpad_pending_guard::CrashpadCleanupReport::default(),
                    snapshot: crashpad_pending_guard::snapshot_system(false),
                }
            }
        })
        .await;
        match result {
            Ok(run) => {
                if !run.cleanup.errors.is_empty() || run.cleanup.still_over_limit {
                    error_log::record_failure(
                        "cleanup_failed",
                        "refresh_crashpad_pending_protection",
                        if run.cleanup.still_over_limit {
                            "Crashpad pending 仍超过安全上限".to_string()
                        } else {
                            format!(
                                "{} 个 Crashpad 待处理文件未能完成收敛",
                                run.cleanup.errors.len()
                            )
                        },
                        json!({
                            "errorCount": run.cleanup.errors.len(),
                            "stillOverLimit": run.cleanup.still_over_limit,
                            "bytesReclaimed": run.cleanup.bytes_reclaimed,
                        }),
                    );
                }
                stats.replace(run.snapshot);
            }
            Err(error) => {
                let mut snapshot = CrashpadPendingStatsSnapshot::idle(protection_enabled);
                snapshot
                    .errors
                    .push(format!("Crashpad 磁盘保护任务异常退出：{error}"));
                stats.replace(snapshot);
            }
        }
    });
}

fn subagent_hot_reload_commit_is_current(
    shutting_down: bool,
    restart_in_progress: bool,
    captured_generation: u64,
    current_generation: u64,
    same_runtime: bool,
    config_matches: bool,
    has_startup_error: bool,
) -> bool {
    !shutting_down
        && !restart_in_progress
        && captured_generation == current_generation
        && same_runtime
        && config_matches
        && !has_startup_error
}

pub(super) async fn hot_reload_runtime_subagent_config(
    state: &Arc<AppState>,
    config: &CodeyConfig,
) -> Option<Result<(), String>> {
    let runtime = state.runtime.lock().await.clone()?;
    if !runtime.supports_subagent_config_hot_reload(config) {
        return None;
    }
    let desired_config = RuntimeSubagentConfig::from_config(config);
    if runtime.applied_subagent_config().await == desired_config {
        return None;
    }
    let runtime_generation = state.runtime_generation.load(Ordering::Acquire);
    // Runtime role files are small and each individual write is atomic. Hold
    // the lifecycle lock across the group so stop/restart cannot swap the lease
    // while it is being committed.
    let _runtime_operation = state.runtime_operation.lock().await;
    let current_runtime = state.runtime.lock().await.clone();
    let same_runtime = current_runtime
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &runtime));
    let current_generation = state.runtime_generation.load(Ordering::Acquire);
    let current_config = state.config.read().await;
    let config_matches = current_config.subagent_optimization
        && RuntimeSubagentConfig::from_config(&current_config) == desired_config
        && current_config.fast_context_tools == config.fast_context_tools;
    drop(current_config);
    let has_startup_error = state.startup_error.read().await.is_some();
    if !subagent_hot_reload_commit_is_current(
        state.is_shutting_down(),
        state.restart_in_progress.load(Ordering::Acquire),
        runtime_generation,
        current_generation,
        same_runtime,
        config_matches,
        has_startup_error,
    ) {
        return Some(Err(
            "Codex 运行时在子代理配置热更新前发生变化；已跳过过期配置".to_string(),
        ));
    }

    let runtime_config = config.clone();
    let result = tokio::task::spawn_blocking(move || {
        refresh_runtime_subagent_roles(&runtime_config).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| format!("子代理运行时文件更新任务异常退出：{error}"))
    .and_then(std::convert::identity);
    match result {
        Ok(()) => {
            let current_runtime = state.runtime.lock().await.clone();
            let same_runtime = current_runtime
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &runtime));
            let current_generation = state.runtime_generation.load(Ordering::Acquire);
            let current_config = state.config.read().await;
            let config_matches = current_config.subagent_optimization
                && RuntimeSubagentConfig::from_config(&current_config) == desired_config
                && current_config.fast_context_tools == config.fast_context_tools;
            drop(current_config);
            let has_startup_error = state.startup_error.read().await.is_some();
            if !subagent_hot_reload_commit_is_current(
                state.is_shutting_down(),
                state.restart_in_progress.load(Ordering::Acquire),
                runtime_generation,
                current_generation,
                same_runtime,
                config_matches,
                has_startup_error,
            ) {
                return Some(Err(
                    "Codex 运行时在子代理配置热更新期间发生变化；已跳过过期运行时提交".to_string(),
                ));
            }
            runtime.mark_subagent_config_applied(config).await;
            Some(Ok(()))
        }
        Err(error) => {
            let error = format!("{error:#}");
            error_log::record_failure(
                "patch_verification_failed",
                "refresh_subagent_runtime_files",
                error.clone(),
                json!({
                    "roleCount": config.subagent_roles.len(),
                }),
            );
            Some(Err(error))
        }
    }
}

#[cfg(test)]
mod subagent_hot_reload_commit_tests;

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
    public.prompt_optimization.api_key_configured =
        !public.prompt_optimization.api_key.trim().is_empty();
    public.prompt_optimization.api_key.clear();
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

    let home = codex_home();
    let mut cache = state.account_usage_cache.lock().await;
    match cache.fetch(&home).await {
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
        || applied.gpu_launch_mode != current.gpu_launch_mode
        || applied.fast_context_tools != current.fast_context_tools
        || applied.fast_codex_startup != current.fast_codex_startup
        || applied.subagent_optimization != current.subagent_optimization
        || applied_models != &RuntimeModelConfig::from_config(current)
        || ((applied.subagent_optimization || current.subagent_optimization)
            && applied_subagent != &RuntimeSubagentConfig::from_config(current))
}

fn model_catalog_config_for_runtime<'a>(
    current: &'a CodeyConfig,
    runtime_applied: Option<&'a CodeyConfig>,
) -> &'a CodeyConfig {
    runtime_applied
        .filter(|applied| provider_route_requires_restart(applied, current))
        .unwrap_or(current)
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
mod restart_tests;

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
    let result = tokio::task::spawn_blocking(move || {
        delete_messages_persistently(&home, &session_id, &message_ids)
    })
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
mod tests;
