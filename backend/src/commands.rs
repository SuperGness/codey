use std::collections::HashMap;
#[cfg(all(test, windows))]
use std::fs;
#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
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
mod wechat_claw;

#[cfg(windows)]
use codey_runtime_core::app_paths::{
    build_codex_executable, normalize_codex_app_path, resolve_codex_app_dir_with_saved,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, RwLock, oneshot, watch};

use diagnostics::{
    clear_diagnostic_storage, refresh_diagnostic_storage_stats, refresh_trace_log_stats,
};
#[cfg(test)]
use models::{
    config_with_current_provider_models, preserve_selected_third_party_models,
    preserve_selected_third_party_models_except, provider_route_requires_restart,
    renderer_model_catalog_value, should_refresh_model_catalog,
    startup_model_sync_models_or_fallback, sync_provider_state_with,
    validate_deleted_third_party_models, validate_manual_model_selection,
};
use models::{
    current_model_state_async, current_provider_status_async, current_renderer_model_catalog_async,
    hot_reload_runtime_models, official_route_snapshots, reconcile_subagent_models_for_mode,
    remote_compaction_transport_requires_restart, runtime_supports_current_routes_for_hot_reload,
    sync_current_third_party_provider_state, sync_provider_models_for_launch,
    websocket_transport_requires_restart,
};
pub use models::{
    delete_route, fetch_route_models, save_default_model, save_official_route_models,
    save_selected_models, sync_current_provider_command,
};
use plugins::{plugin_marketplace_status, repair_plugin_marketplace};
use prompt_optimization::{
    fetch_prompt_optimization_models_command, optimize_prompt_command,
    test_prompt_optimization_command,
};
use runtime::runtime_status_with_options;
#[cfg(test)]
use runtime::{begin_shutdown, launch_codey_inner};
pub use runtime::{
    launch_codey_runtime, runtime_status, schedule_restart_codey_runtime, stop_codey_runtime,
};
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
    sync_waiting_webhook_watcher, test_notification_channel,
};
use wechat_claw::{
    WechatClawLoginState, WechatClawSessionGuard, WechatClawSyncHandle,
    pause_wechat_claw_notification_channel, poll_wechat_claw_login,
    refresh_wechat_claw_channel_context, start_wechat_claw_login, stop_wechat_claw_service,
    sync_wechat_claw_service, wechat_claw_login_http_client,
    wechat_claw_notification_cooldown_remaining,
};

use crate::account_usage;
use crate::cdp;
use crate::codex_config::{
    FastContextToolsStatus, codex_home, fast_context_tools_status, reconcile_runtime_subagent_roles,
};
use crate::codex_provider;
use crate::codex_provider::OfficialAccountProfileStatus;
use crate::config::{
    CodeyConfig, ConfigStore, LaunchOfficialAccountStatus, PromptOptimizationConfig,
    SUBAGENT_ROLE_DEFAULT, SUBAGENT_ROLE_IDS, SubagentRoleConfig, validate_provider_profiles,
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
use crate::model_id;
use crate::notifications::NotificationChannelConfig;
use crate::pending_approval;
use crate::plugin_marketplace;
use crate::route_request_log::RouteRequestLogQuery;
use crate::route_request_log::RouteRequestLogReconfigure;
use crate::session_delete;
use crate::session_metadata;
use crate::session_transfer;
use crate::trace_log_guard;
use crate::trace_log_stats::TraceLogStatsHandle;

const STARTUP_PROVIDER_MODEL_SYNC_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_VISIBLE_SESSION_TIMESTAMPS: usize = 200;

pub struct AppState {
    pub store: ConfigStore,
    pub config: RwLock<CodeyConfig>,
    config_write_lock: Mutex<()>,
    provider_model_sync_lock: Mutex<()>,
    pub http_client: reqwest::Client,
    #[cfg(test)]
    pub webhook_http_client_override: Option<reqwest::Client>,
    wechat_claw_login_http_client: reqwest::Client,
    account_usage_cache: Mutex<account_usage::AccountUsageCache>,
    pub runtime: Mutex<Option<Arc<CodeyRuntime>>>,
    runtime_operation: Mutex<()>,
    diagnostic_storage_operation: Mutex<()>,
    pub trace_log_stats: TraceLogStatsHandle,
    trace_log_write_protection_active: AtomicBool,
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
    #[cfg(test)]
    session_metadata_cache_contended: Notify,
    webhook_notifications: Mutex<WebhookNotificationState>,
    persisted_waiting_notifications: Mutex<WaitingLedgerState>,
    recent_session_event_cache: Mutex<Option<pending_approval::RecentSessionEventCache>>,
    wechat_claw_logins: Mutex<WechatClawLoginState>,
    wechat_claw_sync: Mutex<Option<WechatClawSyncHandle>>,
    wechat_claw_sync_update: Mutex<()>,
    wechat_claw_session_guard: Mutex<WechatClawSessionGuard>,
    waiting_watcher_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    waiting_watcher_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    waiting_watcher_sync: Mutex<()>,
    session_scan_wake: Notify,
    restart_settled: Notify,
    #[cfg(test)]
    restart_operation_pending: Notify,
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
        let (config, config_load_error) = match store.load() {
            Ok(config) => (config, None),
            Err(error) => (
                CodeyConfig::default(),
                Some(format!(
                    "Codey 配置无法读取，已使用安全默认值启动；请先检查或恢复配置文件：{error:#}"
                )),
            ),
        };
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
            #[cfg(test)]
            webhook_http_client_override: None,
            wechat_claw_login_http_client: wechat_claw_login_http_client(),
            account_usage_cache: Mutex::new(account_usage::AccountUsageCache::default()),
            runtime: Mutex::new(None),
            runtime_operation: Mutex::new(()),
            diagnostic_storage_operation: Mutex::new(()),
            trace_log_stats: TraceLogStatsHandle::idle(),
            trace_log_write_protection_active: AtomicBool::new(false),
            crashpad_pending_stats: CrashpadPendingStatsHandle::idle(protect_crashpad_pending),
            startup_error: RwLock::new(config_load_error),
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
            #[cfg(test)]
            session_metadata_cache_contended: Notify::new(),
            webhook_notifications: Mutex::new(WebhookNotificationState::from_settled(
                persisted_waiting_notifications.iter().cloned(),
            )),
            persisted_waiting_notifications: Mutex::new(persisted_waiting_notifications),
            recent_session_event_cache: Mutex::new(Some(
                pending_approval::RecentSessionEventCache::default(),
            )),
            wechat_claw_logins: Mutex::new(WechatClawLoginState::default()),
            wechat_claw_sync: Mutex::new(None),
            wechat_claw_sync_update: Mutex::new(()),
            wechat_claw_session_guard: Mutex::new(WechatClawSessionGuard::default()),
            waiting_watcher_shutdown: Mutex::new(None),
            waiting_watcher_task: Mutex::new(None),
            waiting_watcher_sync: Mutex::new(()),
            session_scan_wake: Notify::new(),
            restart_settled: Notify::new(),
            #[cfg(test)]
            restart_operation_pending: Notify::new(),
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

fn bridge_string_array(payload: &Value, name: &str, limit: usize) -> Vec<String> {
    payload
        .get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(limit)
        .map(ToString::to_string)
        .collect()
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
                    .expect("CodeyConfig must be JSON-serializable")
            }
            "/codex-model-catalog" => {
                let current_config = self.config.read().await.clone();
                let runtime = self.runtime.lock().await.clone();
                let catalog_config = model_catalog_config_for_runtime(
                    &current_config,
                    runtime.as_ref().map(|runtime| &runtime.applied_config),
                )
                .clone();
                current_renderer_model_catalog_async(catalog_config)
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
            "/backend/health" => json!({"status":"ok"}),
            "/account/usage" => account_usage_snapshot(self).await,
            "/session/wake-watcher" => {
                self.session_scan_wake.notify_one();
                json!({"status":"ok"})
            }
            "/session/titles" => cache_session_titles(self, &payload).await,
            "/session/timestamps" => {
                let session_ids =
                    bridge_string_array(&payload, "sessionIds", MAX_VISIBLE_SESSION_TIMESTAMPS);
                let home = codex_home();
                match with_session_metadata_cache(
                    self,
                    "读取侧边栏会话时间",
                    move |cache| cache.resolve_session_timestamps(home, &session_ids),
                )
                .await
                {
                    Ok(timestamps) => json!({"status":"ok", "timestamps": timestamps}),
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
                    session_transfer::start_export_transfer(home, &session_id)
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
                    session_transfer::read_export_transfer_chunk(home, &transfer_id, offset)
                })
                .await
            }
            "/session/export/finish" | "/session/export/abort" => {
                let transfer_id = bridge_string(&payload, "transferId");
                let home = codex_home();
                blocking_value("清理会话导出", move || {
                    session_transfer::finish_export_transfer(home, &transfer_id)?;
                    Ok(json!({"status": "ok"}))
                })
                .await
            }
            "/session/import/start" => {
                let home = codex_home();
                blocking_value("准备会话导入", move || {
                    session_transfer::start_import_transfer(home)
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
                        home,
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
                    session_transfer::finish_import_transfer(home, &project_path, &transfer_id)
                })
                .await
            }
            "/session/import/abort" => {
                let transfer_id = bridge_string(&payload, "transferId");
                let home = codex_home();
                blocking_value("清理会话导入", move || {
                    session_transfer::abort_import_transfer(home, &transfer_id)?;
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
                let plugins_home = home;
                match tokio::task::spawn_blocking(move || {
                    plugin_marketplace::list_plugins(plugins_home)
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
        #[cfg(test)]
        let mut cache = match state.session_metadata_cache.try_lock() {
            Ok(cache) => cache,
            Err(std::sync::TryLockError::WouldBlock) => {
                state.session_metadata_cache_contended.notify_one();
                state
                    .session_metadata_cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        #[cfg(not(test))]
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

const LOCAL_ROUTE_CONFIG_READ_ONLY_ERROR: &str =
    "本地路由已关闭，本地线路配置当前为只读；请先启用本地路由";

pub(super) fn ensure_local_route_config_writable(config: &CodeyConfig) -> Result<(), String> {
    if config.local_router_enabled {
        Ok(())
    } else {
        Err(LOCAL_ROUTE_CONFIG_READ_ONLY_ERROR.to_string())
    }
}

fn local_route_config_changed(previous: &CodeyConfig, next: &CodeyConfig) -> bool {
    previous.active_profile_id != next.active_profile_id
        || previous.profiles != next.profiles
        || previous.selected_models_by_provider != next.selected_models_by_provider
        || previous.manual_third_party_models_by_provider
            != next.manual_third_party_models_by_provider
        || previous.declared_official_models_by_provider
            != next.declared_official_models_by_provider
        || previous.upstream_models_by_provider != next.upstream_models_by_provider
        || previous.default_model != next.default_model
        || previous.default_model_by_provider != next.default_model_by_provider
        || previous.initial_route_import_completed != next.initial_route_import_completed
}

fn ensure_local_route_config_change_allowed(
    previous: &CodeyConfig,
    next: &CodeyConfig,
) -> Result<(), String> {
    if previous.local_router_enabled && next.local_router_enabled {
        return Ok(());
    }
    if local_route_config_changed(previous, next) {
        return Err(LOCAL_ROUTE_CONFIG_READ_ONLY_ERROR.to_string());
    }
    Ok(())
}

pub(super) fn validate_official_account_config_change(
    previous: &CodeyConfig,
    next: &CodeyConfig,
) -> Result<(), String> {
    if previous.official_account_available_this_launch {
        return Ok(());
    }
    if next
        .active_profile()
        .is_some_and(|profile| profile.official_account)
    {
        return Err(
            "本次 Codex 由 API Key 线路启动，不能启用官方账号线路；请先在 Codex 中完成官方账号登录并重新启动 Codey"
                .to_string(),
        );
    }
    if next.profiles.iter().any(|profile| {
        profile.official_account
            && !previous.profiles.iter().any(|previous_profile| {
                previous_profile.id == profile.id && previous_profile.official_account
            })
    }) {
        return Err(
            "本次 Codex 由 API Key 线路启动，不能新增官方账号线路；请先在 Codex 中完成官方账号登录并重新启动 Codey"
                .to_string(),
        );
    }
    Ok(())
}

pub(super) async fn prepare_routes_for_current_launch(state: &Arc<AppState>) -> Result<(), String> {
    let home = codex_home().to_path_buf();
    let configured_codex_app_path = state.config.read().await.codex_app_path.clone();
    let official_status = tokio::task::spawn_blocking(move || {
        crate::codex_provider::current_official_account_profile_status_for_launch(
            &home,
            &configured_codex_app_path,
        )
    })
    .await
    .map_err(|error| format!("检测 Codex 官方账号登录状态的任务异常退出：{error}"))?
    .map_err(|error| format!("检测 Codex 官方账号登录状态失败：{error:#}"))?;

    let _config_write_guard = state.config_write_lock.lock().await;
    let previous = state.config.read().await.clone();
    if !previous.local_router_enabled {
        let next = read_only_config_for_official_probe(previous, official_status);
        *state.config.write().await = next;
        return Ok(());
    }
    let mut next = route_config_for_official_probe(&previous, official_status)?;

    if persisted_config_changed(&previous, &next) {
        if next.settings_revision == previous.settings_revision {
            next.settings_revision = previous.settings_revision.saturating_add(1);
        }
        save_config_to_store(state, &next)
            .await
            .map_err(|error| format!("保存启动线路准备结果失败：{error}"))?;
    }
    *state.config.write().await = next;
    Ok(())
}

fn read_only_config_for_official_probe(
    mut config: CodeyConfig,
    official_status: OfficialAccountProfileStatus,
) -> CodeyConfig {
    match official_status {
        OfficialAccountProfileStatus::Available(_) => {
            config.official_account_available_this_launch = true;
            config.official_account_status_this_launch = LaunchOfficialAccountStatus::Authenticated;
        }
        OfficialAccountProfileStatus::Unavailable { .. } => {
            config.official_account_available_this_launch = false;
            config.official_account_status_this_launch =
                LaunchOfficialAccountStatus::Unauthenticated;
        }
        OfficialAccountProfileStatus::Unknown { .. } => {
            config.official_account_available_this_launch = false;
            config.official_account_status_this_launch = LaunchOfficialAccountStatus::Unknown;
        }
    }
    config
}

fn route_config_for_official_probe(
    previous: &CodeyConfig,
    official_status: OfficialAccountProfileStatus,
) -> Result<CodeyConfig, String> {
    let mut next = previous.clone();
    match official_status {
        OfficialAccountProfileStatus::Available(official_profile) => {
            next.apply_launch_official_profile(Some(official_profile));
            next.initial_route_import_completed = true;
            next = next.normalize();
            next.official_account_available_this_launch = true;
            next.official_account_status_this_launch = LaunchOfficialAccountStatus::Authenticated;
        }
        OfficialAccountProfileStatus::Unavailable { reason } => {
            next = apply_unavailable_official_probe(next, reason)?;
        }
        OfficialAccountProfileStatus::Unknown { profile, reason } => {
            if should_attempt_official_launch_when_auth_unknown(previous) {
                next.apply_launch_official_profile(Some(profile));
                next.initial_route_import_completed = true;
                next = next.normalize();
                next.official_account_available_this_launch = true;
                next.official_account_status_this_launch = LaunchOfficialAccountStatus::Unknown;
                error_log::record_failure_with_metadata(
                    "official_auth_probe_inconclusive",
                    "prepare_routes_for_current_launch",
                    reason,
                    error_log::FailureMetadata {
                        stage: Some("startup.auth_probe".to_string()),
                        recoverable: Some(true),
                    },
                    official_auth_route_diagnostics(
                        previous,
                        "unknown",
                        "launch_with_official_auth",
                    ),
                );
            } else {
                next.official_account_available_this_launch = false;
                next.official_account_status_this_launch = LaunchOfficialAccountStatus::Unknown;
                error_log::record_failure_with_metadata(
                    "official_auth_probe_inconclusive",
                    "prepare_routes_for_current_launch",
                    reason,
                    error_log::FailureMetadata {
                        stage: Some("startup.auth_probe".to_string()),
                        recoverable: Some(true),
                    },
                    official_auth_route_diagnostics(previous, "unknown", "third_party_route"),
                );
            }
        }
    }
    Ok(next)
}

fn apply_unavailable_official_probe(
    mut next: CodeyConfig,
    reason: String,
) -> Result<CodeyConfig, String> {
    let has_official_route = next.profiles.iter().any(|profile| profile.official_account);
    let fallback = if next.has_third_party_route() {
        "third_party_route"
    } else if has_official_route {
        "startup_blocked"
    } else {
        "no_official_route_configured"
    };
    let diagnostics = official_auth_route_diagnostics(&next, "unauthenticated", fallback);
    if has_official_route {
        if !next.has_third_party_route() {
            let error = format!(
                "当前 Codex 没有可用的官方账号登录，也没有已保存的 API Key 线路；请先在 Codex 中完成官方账号登录，或在 Codey 中添加第三方 API 线路。认证诊断：{reason}"
            );
            error_log::record_failure_with_metadata(
                "official_auth_unavailable",
                "prepare_routes_for_current_launch",
                error.clone(),
                error_log::FailureMetadata {
                    stage: Some("startup.auth_probe".to_string()),
                    recoverable: Some(true),
                },
                diagnostics,
            );
            return Err(error);
        }
        next.apply_launch_official_profile(None);
        next = next.normalize();
    }
    error_log::record_failure_with_metadata(
        "official_auth_unavailable",
        "prepare_routes_for_current_launch",
        reason,
        error_log::FailureMetadata {
            stage: Some("startup.auth_probe".to_string()),
            recoverable: Some(true),
        },
        diagnostics,
    );
    next.official_account_available_this_launch = false;
    next.official_account_status_this_launch = LaunchOfficialAccountStatus::Unauthenticated;
    Ok(next)
}

fn official_auth_route_diagnostics(
    config: &CodeyConfig,
    probe_status: &str,
    fallback: &str,
) -> serde_json::Value {
    let active_profile = config.active_profile();
    let official_profile_count = config
        .profiles
        .iter()
        .filter(|profile| profile.official_account)
        .count();
    let third_party_profile_count = config
        .profiles
        .iter()
        .filter(|profile| !profile.official_account && !profile.is_unconfigured_default())
        .count();
    serde_json::json!({
        "probeStatus": probe_status,
        "fallback": fallback,
        "activeProfileId": active_profile.as_ref().map(|profile| profile.id.clone()),
        "activeProfileOfficial": active_profile.as_ref().map(|profile| profile.official_account),
        "profileCount": config.profiles.len(),
        "officialProfileCount": official_profile_count,
        "thirdPartyProfileCount": third_party_profile_count,
        "hasThirdPartyRoute": config.has_third_party_route(),
        "routerRequiresOpenaiAuth": config.router_requires_openai_auth(),
        "initialRouteImportCompleted": config.initial_route_import_completed,
        "officialAccountAvailableBeforeProbe": config.official_account_available_this_launch,
        "officialAccountStatusBeforeProbe": config.official_account_status_this_launch,
        "credentialsIncluded": false,
    })
}

fn should_attempt_official_launch_when_auth_unknown(config: &CodeyConfig) -> bool {
    if config.looks_like_empty_default_route() {
        return true;
    }
    if config
        .active_profile()
        .is_some_and(|profile| profile.official_account)
    {
        return true;
    }
    if !config.has_third_party_route() {
        return true;
    }
    let Some(default_model) = config.default_model() else {
        return false;
    };
    config.profiles.iter().any(|profile| {
        profile.official_account
            && default_model
                .starts_with(&crate::local_router::model_alias(profile.provider_id(), ""))
    })
}

fn persisted_config_changed(previous: &CodeyConfig, next: &CodeyConfig) -> bool {
    let mut previous = previous.clone();
    let mut next = next.clone();
    previous.official_account_available_this_launch = false;
    next.official_account_available_this_launch = false;
    previous.official_account_status_this_launch = LaunchOfficialAccountStatus::Unauthenticated;
    next.official_account_status_this_launch = LaunchOfficialAccountStatus::Unauthenticated;
    previous != next
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
        "delete_route" => match (
            string_argument(&args, "routeId"),
            argument::<u64>(&args, "expectedRevision"),
        ) {
            (Ok(route_id), Ok(expected_revision)) => {
                delete_route(state, route_id, expected_revision).await
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "fetch_route_models" => match (
            string_argument(&args, "routeId"),
            argument::<u64>(&args, "expectedRevision"),
        ) {
            (Ok(route_id), Ok(expected_revision)) => {
                fetch_route_models(state, route_id, expected_revision).await
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "save_selected_models" => match (
            argument::<Vec<String>>(&args, "officialModels"),
            argument::<Vec<String>>(&args, "thirdPartyModels"),
            optional_argument::<Vec<String>>(&args, "manualThirdPartyModels"),
            optional_argument::<Vec<String>>(&args, "deletedThirdPartyModels"),
            optional_argument::<bool>(&args, "supportsAutoReview"),
            optional_argument::<Option<String>>(&args, "routeId").map(Option::flatten),
        ) {
            (
                Ok(official_models),
                Ok(third_party_models),
                Ok(manual_third_party_models),
                Ok(deleted_third_party_models),
                Ok(supports_auto_review),
                Ok(route_id),
            ) => {
                save_selected_models(
                    state,
                    official_models,
                    third_party_models,
                    manual_third_party_models.unwrap_or_default(),
                    deleted_third_party_models.unwrap_or_default(),
                    supports_auto_review,
                    route_id,
                )
                .await
            }
            (Err(error), _, _, _, _, _)
            | (_, Err(error), _, _, _, _)
            | (_, _, Err(error), _, _, _)
            | (_, _, _, Err(error), _, _)
            | (_, _, _, _, Err(error), _)
            | (_, _, _, _, _, Err(error)) => Err(error),
        },
        "save_default_model" => match (
            string_argument(&args, "model"),
            optional_argument::<Option<String>>(&args, "routeId").map(Option::flatten),
        ) {
            (Ok(model), Ok(route_id)) => save_default_model(state, model, route_id).await,
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "save_official_route_models" => match (
            string_argument(&args, "routeId"),
            argument::<Vec<String>>(&args, "models"),
        ) {
            (Ok(route_id), Ok(models)) => save_official_route_models(state, route_id, models).await,
            (Err(error), _) | (_, Err(error)) => Err(error),
        },
        "runtime_status" => {
            let refresh_injection_status = args
                .get("refreshInjectionStatus")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            runtime_status_with_options(state, refresh_injection_status).await
        }
        "refresh_diagnostic_storage_stats" => refresh_diagnostic_storage_stats(state).await,
        "refresh_trace_log_stats" => refresh_trace_log_stats(state).await,
        "open_route_request_logs" => open_route_request_logs(state).await,
        "query_route_request_logs" => {
            match serde_json::from_value::<RouteRequestLogQuery>(args.clone()) {
                Ok(query) => query_route_request_logs(state, query).await,
                Err(error) => Err(format!("请求日志查询参数无效：{error}")),
            }
        }
        "clear_route_request_logs" => clear_route_request_logs(state).await,
        "restart_codey" => schedule_restart_codey_runtime(state).await,
        "clear_diagnostic_storage" => clear_diagnostic_storage(state).await,
        "test_notification_channel" => {
            match argument::<NotificationChannelConfig>(&args, "channel") {
                Ok(channel) => test_notification_channel(state, channel).await,
                Err(error) => Err(error),
            }
        }
        "start_wechat_claw_login" => start_wechat_claw_login(state).await,
        "poll_wechat_claw_login" => match string_argument(&args, "loginId") {
            Ok(login_id) => poll_wechat_claw_login(state, login_id).await,
            Err(error) => Err(error),
        },
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

pub async fn open_route_request_logs(state: &Arc<AppState>) -> Result<Value, String> {
    let endpoint = state
        .runtime
        .lock()
        .await
        .as_ref()
        .and_then(|runtime| runtime.local_router_endpoint())
        .ok_or_else(|| "本地路由尚未运行，无法打开请求日志".to_string())?;
    let url = endpoint.request_log_url();
    tokio::task::spawn_blocking(move || open_system_browser(&url))
        .await
        .map_err(|error| format!("打开系统浏览器任务异常退出：{error}"))??;
    Ok(json!({"status":"ok"}))
}

fn open_system_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");

    command
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法使用系统默认浏览器打开请求日志：{error}"))
}

pub async fn query_route_request_logs(
    state: &Arc<AppState>,
    query: RouteRequestLogQuery,
) -> Result<Value, String> {
    let backend = state.config.read().await.route_request_log.backend;
    let root = codey_runtime_core::paths::default_app_state_dir();
    let page = tokio::task::spawn_blocking(move || {
        crate::route_request_log::query_route_request_logs(&root, backend, query)
    })
    .await
    .map_err(|error| format!("请求日志查询任务异常退出：{error}"))?
    .map_err(|error| format!("查询请求日志失败：{error:#}"))?;
    serde_json::to_value(page).map_err(|error| format!("请求日志查询结果序列化失败：{error}"))
}

pub async fn clear_route_request_logs(state: &Arc<AppState>) -> Result<Value, String> {
    // Keep the runtime decision stable while the controller serializes the
    // writer shutdown/delete/restart sequence. This does not stop or lock the
    // request forwarding task itself.
    let _runtime_operation = state.runtime_operation.lock().await;
    let runtime = state.runtime.lock().await.clone();
    let recording_enabled = {
        let config = state.config.read().await;
        config.route_request_log.enabled
            && config.route_request_log.sample_rate_per_million > 0
            && config.local_router_enabled
    };
    let result = if let Some(runtime) = runtime
        && let Some(result) = runtime.clear_request_logs().await
    {
        result
    } else {
        let root = codey_runtime_core::paths::default_app_state_dir();
        match tokio::task::spawn_blocking(move || {
            crate::route_request_log::clear_route_request_log_files(&root, recording_enabled)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => crate::route_request_log::RouteRequestLogClearResult::failed(
                recording_enabled,
                format!("请求日志清理任务异常退出：{error}"),
            ),
        }
    };
    serde_json::to_value(result).map_err(|error| format!("请求日志清理结果序列化失败：{error}"))
}

pub async fn load_codey_config(state: &Arc<AppState>) -> Result<Value, String> {
    let runtime_running = state.runtime.lock().await.is_some();
    if !runtime_running && let Err(error) = prepare_routes_for_current_launch(state).await {
        error_log::record_failure(
            "route_prepare_failed",
            "load_codey_config",
            error,
            json!({}),
        );
    }
    let imported = ensure_default_route_imported(state).await;
    let config = if imported {
        sync_provider_models_for_launch(state, true).await
    } else {
        state.config.read().await.clone()
    };
    let startup_error = state.startup_error.read().await.clone();
    let provider_status = current_provider_status_async(&config).await?;
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
        "officialAccountAvailable": config.official_account_available_this_launch,
        "officialAccountStatus": config.official_account_status_this_launch,
        "providerStatus": provider_status,
        "modelState": model_state,
        "fastContextToolsStatus": fast_context_tools_status,
    }))
}

pub(super) async fn ensure_default_route_imported(state: &Arc<AppState>) -> bool {
    let config = state.config.read().await.clone();
    if !config.local_router_enabled
        || !config.needs_initial_route_import()
        || config.official_account_available_this_launch
    {
        return false;
    }
    let current_provider = match current_codex_provider_for_initial_import().await {
        Ok(provider) => provider,
        Err(error) => {
            error_log::record_failure(
                "route_import_failed",
                "ensure_default_route_imported",
                error,
                json!({}),
            );
            return false;
        }
    };
    if current_provider.official {
        return false;
    }
    match sync_current_third_party_provider_state(state).await {
        Ok(status) => {
            if !status.changed {
                let _ = mark_initial_route_import_completed(state).await;
            }
            status.changed
        }
        Err(error) => {
            error_log::record_failure(
                "route_import_failed",
                "ensure_default_route_imported",
                error,
                json!({
                    "providerId": current_provider.id,
                    "providerName": current_provider.name,
                }),
            );
            false
        }
    }
}

async fn current_codex_provider_for_initial_import()
-> Result<codex_provider::CurrentProvider, String> {
    let home = codex_home().to_path_buf();
    tokio::task::spawn_blocking(move || codex_provider::current_provider(&home))
        .await
        .map_err(|error| format!("读取当前 Codex 线路任务异常退出：{error}"))?
        .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))
}

async fn mark_initial_route_import_completed(state: &Arc<AppState>) -> Result<bool, String> {
    let _config_write_guard = state.config_write_lock.lock().await;
    let previous = state.config.read().await.clone();
    ensure_local_route_config_writable(&previous)?;
    if previous.initial_route_import_completed {
        return Ok(false);
    }
    let mut next = previous.clone();
    next.initial_route_import_completed = true;
    next.settings_revision = previous.settings_revision.saturating_add(1);
    save_config_to_store(state, &next)
        .await
        .map_err(|error| format!("保存首次线路导入标记失败：{error}"))?;
    *state.config.write().await = next;
    Ok(true)
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
    local_router_enabled_present: bool,
    route_request_log_present: bool,
    subagent_roles_present: bool,
    subagent_model_present: bool,
    subagent_reasoning_effort_present: bool,
}

#[cfg(test)]
impl CodeyConfigSaveInput {
    fn complete(config: CodeyConfig) -> Self {
        Self {
            config,
            local_router_enabled_present: true,
            route_request_log_present: true,
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
    let local_router_enabled_present = fields.contains_key("localRouterEnabled");
    let route_request_log_present = fields.contains_key("routeRequestLog");
    let subagent_roles_present = fields.contains_key("subagentRoles");
    let subagent_model_present = fields.contains_key("subagentModel");
    let subagent_reasoning_effort_present = fields.contains_key("subagentReasoningEffort");
    let config = serde_json::from_value(config_value)
        .map_err(|error| format!("参数 config 无效：{error}"))?;
    Ok(CodeyConfigSaveInput {
        config,
        local_router_enabled_present,
        route_request_log_present,
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
    reconcile_subagent_config: bool,
    fast_context_tools_status: FastContextToolsStatus,
}

async fn save_codey_config_locked(
    state: &Arc<AppState>,
    input: CodeyConfigSaveInput,
) -> Result<SavedCodeyConfig, String> {
    let CodeyConfigSaveInput {
        config: mut config_input,
        local_router_enabled_present,
        route_request_log_present,
        subagent_roles_present,
        subagent_model_present,
        subagent_reasoning_effort_present,
    } = input;
    let previous = state.config.read().await.clone();
    if config_input.settings_revision != previous.settings_revision {
        return Err("Codey 设置已被其他操作更新，请关闭后重新打开设置页面再保存".to_string());
    }
    let mut config = previous.clone();
    config.profiles = merge_profile_secrets(config_input.profiles, &previous)?;
    config.active_profile_id = config_input.active_profile_id;
    if local_router_enabled_present {
        config.local_router_enabled = config_input.local_router_enabled;
    }
    if route_request_log_present {
        config.route_request_log = config_input.route_request_log;
    }
    // Native providers can have model selections without a saved Codey route.
    // Do not prune these caches during read-only saves or router transitions.
    if previous.local_router_enabled && config.local_router_enabled {
        retain_route_scoped_config(&mut config);
    }
    ensure_local_route_config_change_allowed(&previous, &config)?;
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
    config.subagent_optimization = config_input.subagent_optimization;
    let mut explicitly_configured_subagent_models = Vec::new();
    let default_role_supplied = subagent_roles_present
        && !config_input.subagent_roles.is_empty()
        && config_input
            .subagent_roles
            .contains_key(SUBAGENT_ROLE_DEFAULT);
    if subagent_roles_present && !config_input.subagent_roles.is_empty() {
        for (role, selection) in config_input.subagent_roles {
            if SUBAGENT_ROLE_IDS.contains(&role.as_str()) {
                let selection_changed = config.subagent_roles.get(&role).is_none_or(|previous| {
                    previous.enabled != selection.enabled
                        || !model_id::equal(&previous.model, &selection.model)
                        || !previous
                            .reasoning_effort
                            .trim()
                            .eq_ignore_ascii_case(selection.reasoning_effort.trim())
                });
                if selection_changed {
                    explicitly_configured_subagent_models.push(selection.model.clone());
                }
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
            .or_insert_with(|| {
                SubagentRoleConfig::new(fallback_model.clone(), fallback_effort.clone())
            });
        if subagent_model_present {
            default_role.model = config_input.subagent_model;
        }
        if subagent_reasoning_effort_present {
            default_role.reasoning_effort = config_input.subagent_reasoning_effort;
        }
        let default_changed = !model_id::equal(&fallback_model, &default_role.model)
            || !fallback_effort
                .trim()
                .eq_ignore_ascii_case(default_role.reasoning_effort.trim());
        if default_changed {
            explicitly_configured_subagent_models.push(default_role.model.clone());
        }
    }
    config.hide_full_access_warning = config_input.hide_full_access_warning;
    config.show_account_usage_in_header = config_input.show_account_usage_in_header;
    let mut config = config.normalize();
    validate_official_account_config_change(&previous, &config)?;
    if previous.local_router_enabled && config.local_router_enabled {
        config.remember_current_provider_official_model_support(
            explicitly_configured_subagent_models,
        );
    }
    config = config.normalize();
    // The input was checked before normalization. Re-enabling can migrate
    // native model selections back into the router's legacy model maps.
    if !config.local_router_enabled {
        ensure_local_route_config_change_allowed(&previous, &config)?;
    }
    if (config.subagent_optimization
        || (previous.local_router_enabled && !config.local_router_enabled))
        && let Ok(model_state) = current_model_state_async(&config).await
    {
        reconcile_subagent_models_for_mode(&mut config, &model_state);
        config = config.normalize();
    }
    // Codex reads each registered role config_file again when spawning a child.
    // Check the Codey-owned runtime files on every save while the policy stays
    // enabled, even when the in-memory role summary did not change. Enabling or
    // disabling still requires a restart to register/unregister tools and hooks.
    let reconcile_subagent_config = should_reconcile_runtime_subagent_config(&previous, &config);
    config.settings_revision = previous.settings_revision.saturating_add(1);
    let trace_guard_changed = config.disable_trace_log_writes != previous.disable_trace_log_writes;
    let _diagnostic_operation = if trace_guard_changed {
        Some(state.diagnostic_storage_operation.lock().await)
    } else {
        None
    };
    let trace_guard_report = if trace_guard_changed {
        let home = codex_home().to_path_buf();
        let disable_writes = config.disable_trace_log_writes;
        match configure_trace_log_guard(home.clone(), disable_writes).await {
            Ok(report) => Some(report),
            Err(error) => {
                let error =
                    rollback_trace_log_guard(home, previous.disable_trace_log_writes, error).await;
                state
                    .trace_log_write_protection_active
                    .store(false, Ordering::Release);
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
    } else {
        None
    };
    if let Err(error) = save_config_to_store(state, &config).await {
        if trace_guard_changed {
            let error = rollback_trace_log_guard(
                codex_home().to_path_buf(),
                previous.disable_trace_log_writes,
                error,
            )
            .await;
            state
                .trace_log_write_protection_active
                .store(false, Ordering::Release);
            return Err(error);
        }
        return Err(error);
    }
    *state.config.write().await = config.clone();
    if let Some(report) = trace_guard_report {
        state.trace_log_write_protection_active.store(
            report.protection_active(config.disable_trace_log_writes),
            Ordering::Release,
        );
    }
    Ok(SavedCodeyConfig {
        config,
        reconcile_subagent_config,
        fast_context_tools_status,
    })
}

fn merge_profile_secrets(
    mut profiles: Vec<crate::config::ProviderProfile>,
    previous: &CodeyConfig,
) -> Result<Vec<crate::config::ProviderProfile>, String> {
    let previous_by_id = previous
        .profiles
        .iter()
        .map(|profile| (profile.id.as_str(), profile))
        .collect::<std::collections::HashMap<_, _>>();
    for profile in &mut profiles {
        let previous_profile = previous_by_id.get(profile.id.as_str()).copied();
        profile.merge_redacted_secret(previous_profile);
        if let Some(previous_profile) = previous_profile {
            let auth_mode_changed = !profile
                .auth_mode
                .trim()
                .eq_ignore_ascii_case(previous_profile.auth_mode.trim());
            if auth_mode_changed {
                profile.model_request_headers.clear();
                profile.source_provider_id = None;
                profile.supports_remote_compaction = false;
                // Official routes derive WebSocket support automatically. Do
                // not carry that derived capability into a newly converted
                // API-key route; third-party WebSocket remains explicit opt-in.
                profile.supports_websockets = false;
                profile.supports_auto_review = false;
                if profile.auth_mode.trim() == crate::config::AUTH_MODE_API_KEY {
                    profile.official_account = false;
                }
            } else {
                // These fields are discovered from the trusted Codex source and
                // are not editable renderer input. Keep them attached
                // to the saved route even though the renderer receives a redacted
                // profile and sends the whole form back on save.
                profile.model_request_headers = previous_profile.model_request_headers.clone();
                profile.source_provider_id = previous_profile.source_provider_id.clone();
                profile.official_account = previous_profile.official_account;
                profile.supports_remote_compaction = previous_profile.supports_remote_compaction;
            }
        }
        profile.normalize();
    }
    validate_provider_profiles(&profiles)?;
    Ok(profiles)
}

fn retain_route_scoped_config(config: &mut CodeyConfig) {
    let provider_ids = config
        .profiles
        .iter()
        .map(|profile| {
            profile
                .source_provider_id
                .as_deref()
                .unwrap_or(profile.id.as_str())
                .to_string()
        })
        .collect::<std::collections::HashSet<_>>();
    config
        .selected_models_by_provider
        .retain(|provider_id, _| provider_ids.contains(provider_id));
    config
        .manual_third_party_models_by_provider
        .retain(|provider_id, _| provider_ids.contains(provider_id));
    config
        .declared_official_models_by_provider
        .retain(|provider_id, _| provider_ids.contains(provider_id));
    config
        .upstream_models_by_provider
        .retain(|provider_id, _| provider_ids.contains(provider_id));
}

fn current_fast_context_tools_status() -> FastContextToolsStatus {
    fast_context_tools_status_or_blocked(fast_context_tools_status(codex_home()))
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

async fn configure_trace_log_guard(
    home: PathBuf,
    disable_writes: bool,
) -> Result<trace_log_guard::TraceLogGuardReport, String> {
    tokio::task::spawn_blocking(move || trace_log_guard::configure(&home, disable_writes))
        .await
        .map_err(|error| format!("Trace 日志保护切换任务异常退出：{error}"))?
        .map_err(|error| error.to_string())
}

async fn rollback_trace_log_guard(
    home: PathBuf,
    previous_disable_writes: bool,
    primary_error: String,
) -> String {
    match configure_trace_log_guard(home, previous_disable_writes).await {
        Ok(_) => primary_error,
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
    sync_wechat_claw_service(state).await;
    if let Some(runtime) = state.runtime.lock().await.clone() {
        runtime.set_crashpad_pending_protection(saved.config.protect_crashpad_pending);
    }
    schedule_crashpad_pending_refresh(state, saved.config.protect_crashpad_pending);
    let model_state = current_model_state_async(&saved.config).await?;
    let model_hot_reload = hot_reload_runtime_models(state, &saved.config, &model_state).await;
    let route_request_log_hot_reload = hot_reload_runtime_request_log(state, &saved.config).await;
    let subagent_hot_reload = if saved.reconcile_subagent_config {
        hot_reload_runtime_subagent_config(state, &saved.config).await
    } else {
        SubagentHotReloadOutcome::default()
    };
    let restart_required = subagent_hot_reload.requires_restart()
        || runtime_config_requires_restart(state, &saved.config).await;
    let subagent_config_hot_reloaded = subagent_hot_reload.reloaded();
    let subagent_config_repaired = subagent_hot_reload.repaired();
    let subagent_config_health = subagent_hot_reload.health();
    let subagent_config_repair_reasons = subagent_hot_reload.repair_reasons();
    let subagent_config_hot_reload_error = subagent_hot_reload.error();
    let provider_status = current_provider_status_async(&saved.config).await?;
    let public_config = redacted_config(&saved.config);
    Ok(model_hot_reload.add_to_response(json!({
        "status":"ok",
        "config":public_config,
        "providerStatus":provider_status,
        "modelState":model_state,
        "fastContextToolsStatus":saved.fast_context_tools_status,
        "restartRequired":restart_required,
        "routeRequestLogHotReloaded":route_request_log_hot_reload.reloaded(),
        "routeRequestLogHealth":route_request_log_hot_reload.health(),
        "routeRequestLogHotReloadError":route_request_log_hot_reload.error(),
        "subagentConfigHotReloaded":subagent_config_hot_reloaded,
        "subagentConfigRepaired":subagent_config_repaired,
        "subagentConfigHealth":subagent_config_health,
        "subagentConfigRepairReasons":subagent_config_repair_reasons,
        "subagentConfigHotReloadError":subagent_config_hot_reload_error,
    })))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RouteRequestLogHotReloadStatus {
    #[default]
    NotApplicable,
    Unchanged,
    Enabled,
    Disabled,
    Superseded,
    Failed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RouteRequestLogHotReloadOutcome {
    status: RouteRequestLogHotReloadStatus,
    error: Option<String>,
}

impl RouteRequestLogHotReloadOutcome {
    fn superseded(error: impl Into<String>) -> Self {
        Self {
            status: RouteRequestLogHotReloadStatus::Superseded,
            error: Some(error.into()),
        }
    }

    fn failed(error: impl Into<String>) -> Self {
        Self {
            status: RouteRequestLogHotReloadStatus::Failed,
            error: Some(error.into()),
        }
    }

    fn reloaded(&self) -> bool {
        matches!(
            self.status,
            RouteRequestLogHotReloadStatus::Enabled | RouteRequestLogHotReloadStatus::Disabled
        )
    }

    fn health(&self) -> &'static str {
        match self.status {
            RouteRequestLogHotReloadStatus::NotApplicable => "not_applicable",
            RouteRequestLogHotReloadStatus::Unchanged => "unchanged",
            RouteRequestLogHotReloadStatus::Enabled => "enabled",
            RouteRequestLogHotReloadStatus::Disabled => "disabled",
            RouteRequestLogHotReloadStatus::Superseded => "superseded",
            RouteRequestLogHotReloadStatus::Failed => "failed",
        }
    }

    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

async fn hot_reload_runtime_request_log(
    state: &Arc<AppState>,
    config: &CodeyConfig,
) -> RouteRequestLogHotReloadOutcome {
    let desired = config.route_request_log.clone();
    let _runtime_operation = state.runtime_operation.lock().await;
    let _config_commit_guard = state.config_write_lock.lock().await;
    let current_config = state.config.read().await.clone();
    if current_config.route_request_log != desired
        || current_config.local_router_enabled != config.local_router_enabled
    {
        return RouteRequestLogHotReloadOutcome::superseded(
            "Codey 设置在请求日志热更新前已被更新；已跳过过期配置",
        );
    }
    let Some(runtime) = state.runtime.lock().await.clone() else {
        return RouteRequestLogHotReloadOutcome::default();
    };
    if !runtime.applied_config.local_router_enabled || !current_config.local_router_enabled {
        return RouteRequestLogHotReloadOutcome::default();
    }
    let runtime_generation = state.runtime_generation.load(Ordering::Acquire);
    let current_runtime = state.runtime.lock().await.clone();
    let same_runtime = current_runtime
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &runtime));
    if state.is_shutting_down()
        || state.restart_in_progress.load(Ordering::Acquire)
        || runtime_generation != state.runtime_generation.load(Ordering::Acquire)
        || !same_runtime
        || state.startup_error.read().await.is_some()
    {
        return RouteRequestLogHotReloadOutcome::superseded(
            "Codex 运行时在请求日志热更新前发生变化；已跳过过期配置",
        );
    }

    match runtime.reconfigure_request_log(&desired).await {
        Ok(Some(RouteRequestLogReconfigure::Unchanged)) => RouteRequestLogHotReloadOutcome {
            status: RouteRequestLogHotReloadStatus::Unchanged,
            error: None,
        },
        Ok(Some(RouteRequestLogReconfigure::Enabled)) => RouteRequestLogHotReloadOutcome {
            status: RouteRequestLogHotReloadStatus::Enabled,
            error: None,
        },
        Ok(Some(RouteRequestLogReconfigure::Disabled)) => RouteRequestLogHotReloadOutcome {
            status: RouteRequestLogHotReloadStatus::Disabled,
            error: None,
        },
        Ok(None) => RouteRequestLogHotReloadOutcome::default(),
        Err(error) => {
            let error = format!("请求日志热更新失败：{error:#}");
            error_log::record_failure(
                "route_request_log_hot_reload_failed",
                "reconfigure_route_request_log",
                error.clone(),
                json!({}),
            );
            RouteRequestLogHotReloadOutcome::failed(error)
        }
    }
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SubagentHotReloadStatus {
    #[default]
    NotApplicable,
    Unchanged,
    Applied,
    Repaired,
    Superseded,
    Failed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct SubagentHotReloadOutcome {
    status: SubagentHotReloadStatus,
    error: Option<String>,
    repair_reasons: Vec<String>,
}

impl SubagentHotReloadOutcome {
    fn unchanged() -> Self {
        Self {
            status: SubagentHotReloadStatus::Unchanged,
            ..Self::default()
        }
    }

    fn applied(repaired: bool, repair_reasons: Vec<String>) -> Self {
        Self {
            status: if repaired {
                SubagentHotReloadStatus::Repaired
            } else {
                SubagentHotReloadStatus::Applied
            },
            repair_reasons,
            ..Self::default()
        }
    }

    fn superseded(error: impl Into<String>) -> Self {
        Self {
            status: SubagentHotReloadStatus::Superseded,
            error: Some(error.into()),
            ..Self::default()
        }
    }

    fn failed(error: impl Into<String>) -> Self {
        Self {
            status: SubagentHotReloadStatus::Failed,
            error: Some(error.into()),
            ..Self::default()
        }
    }

    pub(super) fn reloaded(&self) -> bool {
        matches!(
            self.status,
            SubagentHotReloadStatus::Applied | SubagentHotReloadStatus::Repaired
        )
    }

    pub(super) fn repaired(&self) -> bool {
        self.status == SubagentHotReloadStatus::Repaired
    }

    pub(super) fn requires_restart(&self) -> bool {
        self.status == SubagentHotReloadStatus::Failed
    }

    pub(super) fn health(&self) -> &'static str {
        match self.status {
            SubagentHotReloadStatus::NotApplicable => "not_applicable",
            SubagentHotReloadStatus::Unchanged => "healthy",
            SubagentHotReloadStatus::Applied => "applied",
            SubagentHotReloadStatus::Repaired => "repaired",
            SubagentHotReloadStatus::Superseded => "superseded",
            SubagentHotReloadStatus::Failed => "restart_required",
        }
    }

    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(super) fn repair_reasons(&self) -> &[String] {
        &self.repair_reasons
    }
}

fn should_reconcile_runtime_subagent_config(previous: &CodeyConfig, current: &CodeyConfig) -> bool {
    previous.subagent_optimization && current.subagent_optimization
}

pub(super) async fn hot_reload_runtime_subagent_config(
    state: &Arc<AppState>,
    config: &CodeyConfig,
) -> SubagentHotReloadOutcome {
    let desired_config = RuntimeSubagentConfig::from_config(config);

    // All code that needs both locks follows the lifecycle -> config order.
    // Restart already holds the lifecycle lock while launch may synchronize and
    // persist provider state, so taking the config lock first here would allow a
    // save/restart lock inversion. Holding both locks across reconciliation still
    // prevents an older save from committing role files after a newer config.
    let _runtime_operation = state.runtime_operation.lock().await;
    let _config_commit_guard = state.config_write_lock.lock().await;
    let current_config = state.config.read().await.clone();
    let config_matches = current_config.subagent_optimization
        && RuntimeSubagentConfig::from_config(&current_config) == desired_config
        && current_config.fast_context_tools == config.fast_context_tools;
    if !config_matches {
        return SubagentHotReloadOutcome::superseded(
            "Codey 设置在子代理配置热更新前已被更新；已跳过过期配置",
        );
    }
    let Some(runtime) = state.runtime.lock().await.clone() else {
        return SubagentHotReloadOutcome::default();
    };
    if !runtime.supports_subagent_config_hot_reload(&current_config) {
        return SubagentHotReloadOutcome::default();
    }
    let applied_config_changed = runtime.applied_subagent_config().await != desired_config;
    let runtime_generation = state.runtime_generation.load(Ordering::Acquire);
    let current_runtime = state.runtime.lock().await.clone();
    let same_runtime = current_runtime
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &runtime));
    let current_generation = state.runtime_generation.load(Ordering::Acquire);
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
        return SubagentHotReloadOutcome::superseded(
            "Codex 运行时在子代理配置热更新前发生变化；已跳过过期配置",
        );
    }

    let runtime_config = runtime.subagent_reconcile_config(&current_config);
    let result = tokio::task::spawn_blocking(move || {
        reconcile_runtime_subagent_roles(&runtime_config).map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| format!("子代理运行时文件更新任务异常退出：{error}"))
    .and_then(std::convert::identity);
    match result {
        Ok(report) => {
            if !report.repaired && !applied_config_changed {
                return SubagentHotReloadOutcome::unchanged();
            }
            let current_runtime = state.runtime.lock().await.clone();
            let same_runtime = current_runtime
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &runtime));
            let current_generation = state.runtime_generation.load(Ordering::Acquire);
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
                return SubagentHotReloadOutcome::failed(
                    "Codex 运行时在子代理配置热更新期间发生变化；需要重启以重新建立可信运行配置",
                );
            }
            runtime.mark_subagent_config_applied(&current_config).await;
            SubagentHotReloadOutcome::applied(
                report.repaired,
                report
                    .reasons
                    .into_iter()
                    .map(|reason| reason.as_str().to_string())
                    .collect(),
            )
        }
        Err(error) => {
            let error = format!("{error:#}");
            error_log::record_failure(
                "patch_verification_failed",
                "reconcile_subagent_runtime_files",
                error.clone(),
                json!({
                    "roleCount": config.subagent_roles.len(),
                }),
            );
            SubagentHotReloadOutcome::failed(error)
        }
    }
}

#[cfg(test)]
mod subagent_hot_reload_commit_tests;

fn redacted_config(config: &CodeyConfig) -> CodeyConfig {
    let mut public = config.clone();
    for profile in &mut public.profiles {
        profile.api_key_configured = !profile.api_key.trim().is_empty();
    }
    public.webhook.url.clear();
    for channel in &mut public.webhook.channels {
        channel.url_configured = !channel.url.trim().is_empty();
        channel.url.clear();
        channel.bot_token_configured = !channel.bot_token.trim().is_empty();
        channel.bot_token.clear();
        channel.context_token_configured = !channel.context_token.trim().is_empty();
        channel.context_token.clear();
        channel.get_updates_buf.clear();
    }
    public.prompt_optimization.api_key_configured =
        !public.prompt_optimization.api_key.trim().is_empty();
    public
}

async fn account_usage_snapshot(state: &Arc<AppState>) -> Value {
    {
        let config = state.config.read().await;
        if !config.show_account_usage_in_header {
            return json!({"status": "disabled"});
        }
        if !account_usage_enabled_for_config(&config) {
            return json!({
                "status": "unavailable",
                "reason": "official_account_missing",
                "message": "当前线路列表中没有可用的官方账号线路",
            });
        }
    }

    let home = codex_home();
    let mut cache = state.account_usage_cache.lock().await;
    match cache.fetch(home).await {
        Ok(snapshot) => {
            let mut value = serde_json::to_value(snapshot)
                .expect("account usage snapshots must be JSON-serializable");
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

fn account_usage_enabled_for_config(config: &CodeyConfig) -> bool {
    if !config.show_account_usage_in_header {
        return false;
    }
    if !config.local_router_enabled {
        return config.official_account_available_this_launch;
    }
    config
        .profiles
        .iter()
        .any(|profile| profile.official_account)
}

#[cfg(test)]
fn config_requires_restart(
    applied: &CodeyConfig,
    applied_models: &RuntimeModelConfig,
    applied_subagent: &RuntimeSubagentConfig,
    current: &CodeyConfig,
) -> bool {
    config_requires_restart_with_route_status(
        provider_route_requires_restart(applied, current),
        applied,
        applied_models,
        applied_subagent,
        current,
    )
}

pub(super) fn config_requires_restart_with_route_status(
    provider_route_restart_required: bool,
    applied: &CodeyConfig,
    applied_models: &RuntimeModelConfig,
    applied_subagent: &RuntimeSubagentConfig,
    current: &CodeyConfig,
) -> bool {
    provider_route_restart_required
        || applied.codex_app_path != current.codex_app_path
        || applied.user_scripts != current.user_scripts
        || applied.slim_codex_pet != current.slim_codex_pet
        || applied.gpu_launch_mode != current.gpu_launch_mode
        || applied.fast_context_tools != current.fast_context_tools
        || applied.subagent_optimization != current.subagent_optimization
        || applied_models != &RuntimeModelConfig::from_config(current)
        || ((applied.subagent_optimization || current.subagent_optimization)
            && applied_subagent != &RuntimeSubagentConfig::from_config(current))
}

pub(super) fn provider_route_restart_required_for_runtime(
    runtime: &CodeyRuntime,
    current: &CodeyConfig,
) -> bool {
    runtime.applied_config.local_router_enabled != current.local_router_enabled
        || official_route_snapshots(&runtime.applied_config) != official_route_snapshots(current)
        || websocket_transport_requires_restart(&runtime.applied_config, current)
        || remote_compaction_transport_requires_restart(&runtime.applied_config, current)
}

fn model_catalog_config_for_runtime<'a>(
    current: &'a CodeyConfig,
    runtime_applied: Option<&'a CodeyConfig>,
) -> &'a CodeyConfig {
    runtime_applied
        .filter(|applied| !runtime_supports_current_routes_for_hot_reload(applied, current))
        .unwrap_or(current)
}

async fn runtime_config_requires_restart(state: &Arc<AppState>, current: &CodeyConfig) -> bool {
    let runtime = state.runtime.lock().await.clone();
    let Some(runtime) = runtime else {
        return false;
    };
    let applied_models = runtime.applied_model_config().await;
    let applied_subagent = runtime.applied_subagent_config().await;
    let provider_route_restart_required =
        provider_route_restart_required_for_runtime(&runtime, current);
    config_requires_restart_with_route_status(
        provider_route_restart_required,
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
        delete_messages_persistently(home, &session_id, &message_ids)
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
        session_delete::delete_session(home, &session_id, &title)
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
