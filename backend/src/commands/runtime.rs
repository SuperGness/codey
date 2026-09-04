use std::path::PathBuf;
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;

#[cfg(windows)]
use super::ensure_windows_codex_app_path;
use super::webhooks::{
    RecentSessionScanTask, await_recent_session_scan, start_recent_session_scan,
    start_waiting_webhook_watcher, stop_waiting_webhook_watcher, webhook_watcher_should_run,
};
use super::{
    AppState, RestartInProgressGuard, ScheduledRestart, config_requires_restart_with_route_status,
    current_update_platform, make_bridge_handler, prepare_routes_for_current_launch,
    provider_route_restart_required_for_runtime, sync_provider_models_for_launch,
};
use crate::codex_config::codex_home;
use crate::error_log;
use crate::launcher::{CodeyRuntime, restore_previous_runtime_state};

const CODEX_APP_VERSION_CACHE_TTL: Duration = Duration::from_secs(30);

pub(super) struct CodexAppVersionCache {
    runtime_app_path: Option<PathBuf>,
    configured_app_path: String,
    version: String,
    lookup_started_at: Instant,
    checked_at: Instant,
}

fn runtime_feature_status_value(
    fast_context_tools_active: bool,
    subagent_optimization_active: bool,
    active_notification_channel_count: usize,
    trace_log_write_protection_active: bool,
    crashpad_disk_protection_active: bool,
) -> Value {
    json!({
        "fastContextToolsActive": fast_context_tools_active,
        "subagentOptimizationActive": subagent_optimization_active,
        "notificationChannelsActive": active_notification_channel_count > 0,
        "activeNotificationChannelCount": active_notification_channel_count,
        "traceLogWriteProtectionActive": trace_log_write_protection_active,
        "crashpadDiskProtectionActive": crashpad_disk_protection_active,
    })
}

pub async fn runtime_status(state: &Arc<AppState>) -> Result<Value, String> {
    runtime_status_with_options(state, false).await
}

pub(super) async fn runtime_status_with_options(
    state: &Arc<AppState>,
    refresh_injection_status: bool,
) -> Result<Value, String> {
    let runtime = state.runtime.lock().await.clone();
    // 先在无锁状态取运行时模型基线，再持配置读锁做同步比较：读守卫跨
    // await 会让排队写者阻塞所有新的桥接请求。
    let applied_models = match runtime.as_ref() {
        Some(runtime) => Some(runtime.applied_model_config().await),
        None => None,
    };
    let applied_subagent = match runtime.as_ref() {
        Some(runtime) => Some(runtime.applied_subagent_config().await),
        None => None,
    };
    let crashpad_disk_protection_active = match runtime.as_ref() {
        Some(runtime) => runtime.crashpad_pending_protection_active().await,
        None => false,
    };
    let config = state.config.read().await;
    let profile = config.active_profile();
    let active_profile_id = profile
        .as_ref()
        .map(|profile| profile.id.clone())
        .unwrap_or_default();
    let active_profile_name = profile
        .as_ref()
        .map(|profile| profile.name.clone())
        .unwrap_or_default();
    let configured_codex_app_path = config.codex_app_path.clone();
    let official_account_available = config.official_account_available_this_launch;
    let official_account_status = config.official_account_status_this_launch;
    let runtime_codex_app_path = runtime
        .as_ref()
        .map(|runtime| runtime.codex_app_path.clone());
    let restart_required = match (
        runtime.as_ref(),
        applied_models.as_ref(),
        applied_subagent.as_ref(),
    ) {
        (Some(runtime), Some(applied_models), Some(applied_subagent)) => {
            config_requires_restart_with_route_status(
                provider_route_restart_required_for_runtime(runtime, &config),
                &runtime.applied_config,
                applied_models,
                applied_subagent,
                &config,
            )
        }
        _ => false,
    };
    let fast_context_tools_active = runtime
        .as_ref()
        .is_some_and(|runtime| runtime.applied_config.fast_context_tools);
    let subagent_optimization_active = runtime
        .as_ref()
        .is_some_and(|runtime| runtime.applied_config.subagent_optimization);
    let configured_notification_channel_count = config.webhook.enabled_channel_count();
    let trace_log_write_protection_active = state
        .trace_log_write_protection_active
        .load(Ordering::Acquire);
    drop(config);
    let notification_watcher_active = runtime.is_some()
        && state
            .waiting_watcher_task
            .lock()
            .await
            .as_ref()
            .is_some_and(|task| !task.is_finished());
    let active_notification_channel_count = if notification_watcher_active {
        configured_notification_channel_count
    } else {
        0
    };
    let mut status = json!({
        "running": runtime.is_some(),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "clientPlatform": current_update_platform(),
        "activeProfileId": active_profile_id,
        "activeProfileName": active_profile_name,
        "officialAccountAvailable": official_account_available,
        "officialAccountStatus": official_account_status,
        "restartRequired": restart_required,
        "restartInProgress": state.restart_in_progress.load(Ordering::Acquire),
    });
    if let (Some(status), Some(feature_status)) = (
        status.as_object_mut(),
        runtime_feature_status_value(
            fast_context_tools_active,
            subagent_optimization_active,
            active_notification_channel_count,
            trace_log_write_protection_active,
            crashpad_disk_protection_active,
        )
        .as_object(),
    ) {
        status.extend(feature_status.clone());
    }
    let codex_app_version =
        codex_app_version_for_status(state, runtime_codex_app_path, configured_codex_app_path)
            .await;
    if let Some(object) = status.as_object_mut() {
        object.insert("codexAppVersion".into(), Value::String(codex_app_version));
    }
    if let Some(error) = state.startup_error.read().await.clone()
        && let Some(object) = status.as_object_mut()
    {
        object.insert("startupError".into(), Value::String(error));
    };
    if let Some(update) = state.available_update.read().await.clone()
        && let Some(object) = status.as_object_mut()
    {
        object.insert(
            "availableUpdate".into(),
            serde_json::to_value(update).expect("update metadata must be JSON-serializable"),
        );
    }
    if let Some(runtime) = runtime.as_ref()
        && let Some(object) = status.as_object_mut()
    {
        object.insert(
            "codexAppPath".into(),
            Value::String(runtime.codex_app_path.to_string_lossy().to_string()),
        );
        object.insert(
            "maintenance".into(),
            serde_json::to_value(&runtime.maintenance)
                .expect("maintenance status must be JSON-serializable"),
        );
        let injection_statuses = if refresh_injection_status {
            runtime.refresh_injection_statuses().await
        } else {
            runtime.injection_statuses.read().await.clone()
        };
        object.insert(
            "injectionScripts".into(),
            serde_json::to_value(injection_statuses.as_ref())
                .expect("injection statuses must be JSON-serializable"),
        );
    }
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "traceLogStats".into(),
            serde_json::to_value(&state.trace_log_stats)
                .expect("trace log stats must be JSON-serializable"),
        );
        object.insert(
            "crashpadPendingStats".into(),
            serde_json::to_value(&state.crashpad_pending_stats)
                .expect("Crashpad stats must be JSON-serializable"),
        );
    }
    Ok(status)
}

async fn codex_app_version_for_status(
    state: &AppState,
    runtime_app_path: Option<PathBuf>,
    configured_app_path: String,
) -> String {
    let lookup_started_at = Instant::now();
    {
        let cache = state.codex_app_version_cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.runtime_app_path == runtime_app_path
            && cached.configured_app_path == configured_app_path
            && lookup_started_at.saturating_duration_since(cached.checked_at)
                < CODEX_APP_VERSION_CACHE_TTL
        {
            return cached.version.clone();
        }
    }

    // App bundle discovery performs blocking filesystem work. Do not keep the
    // async cache mutex locked while it runs: concurrent status requests should
    // remain independent even when the application lives on a slow volume.
    let checked_runtime_app_path = runtime_app_path.clone();
    let checked_configured_app_path = configured_app_path.clone();
    let version = tokio::task::spawn_blocking(move || {
        error_log::refresh_codex_app_version(
            checked_runtime_app_path.as_deref(),
            Some(&checked_configured_app_path),
        )
        .unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    let mut cache = state.codex_app_version_cache.lock().await;
    if let Some(cached) = cache.as_ref()
        && cached.runtime_app_path == runtime_app_path
        && cached.configured_app_path == configured_app_path
        && cached.lookup_started_at >= lookup_started_at
    {
        // Another request completed the same lookup while this one was doing
        // filesystem work. Reuse the newer result instead of replacing it.
        return cached.version.clone();
    }
    if cache
        .as_ref()
        .is_none_or(|cached| cached.lookup_started_at < lookup_started_at)
    {
        *cache = Some(CodexAppVersionCache {
            runtime_app_path,
            configured_app_path,
            version: version.clone(),
            lookup_started_at,
            checked_at: Instant::now(),
        });
    }
    version
}

fn ensure_runtime_can_start(state: &AppState) -> Result<(), String> {
    if state.is_shutting_down() {
        Err("Codey 正在退出，无法启动 Codex".to_string())
    } else {
        Ok(())
    }
}

async fn reclaim_initial_session_scan(
    state: &Arc<AppState>,
    initial_scan_task: Option<RecentSessionScanTask>,
) {
    if let Some(initial_scan_task) = initial_scan_task {
        let (initial_event_cache, _) = await_recent_session_scan(initial_scan_task).await;
        *state.recent_session_event_cache.lock().await = Some(initial_event_cache);
    }
}

async fn forward_codex_exit_to_codey_shutdown(
    exit_state: Arc<AppState>,
    codex_exit: oneshot::Receiver<()>,
    runtime_generation: u64,
) {
    if codex_exit.await.is_err() {
        return;
    }
    while exit_state.restart_in_progress.load(Ordering::Acquire) {
        let settled = exit_state.restart_settled.notified();
        if !exit_state.restart_in_progress.load(Ordering::Acquire) {
            break;
        }
        // 兜底超时只是防丢通知，正常路径由 RestartInProgressGuard
        // 的析构即时唤醒。
        let _ = tokio::time::timeout(Duration::from_millis(250), settled).await;
    }
    // 重启期间旧 Codex 的退出不能关闭新一代运行时；只有当前受控
    // Codex 的自然退出才联动关闭 Codey 主进程。
    if exit_state.runtime_generation.load(Ordering::Acquire) == runtime_generation {
        exit_state.request_shutdown();
    }
}

async fn launch_codey_inner_locked(state: &Arc<AppState>) -> Result<Value, String> {
    ensure_runtime_can_start(state)?;
    if state.runtime.lock().await.is_some() {
        return Ok(json!({"status":"already_running"}));
    }
    #[cfg(windows)]
    ensure_windows_codex_app_path(state).await?;
    stop_waiting_webhook_watcher(state).await;
    let local_router_enabled = state.config.read().await.local_router_enabled;
    restore_previous_runtime_state(codex_home(), local_router_enabled)
        .await
        .map_err(|error| format!("恢复上次 Codey 临时 Codex 配置失败：{error}"))?;
    prepare_routes_for_current_launch(state).await?;
    let imported_default_route = super::ensure_default_route_imported(state).await;
    let config = sync_provider_models_for_launch(state, imported_default_route).await;
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
    if let Err(error) = ensure_runtime_can_start(state) {
        reclaim_initial_session_scan(state, initial_scan_task).await;
        return Err(error);
    }
    let handler = make_bridge_handler(state);
    let (runtime, codex_exit) = match CodeyRuntime::start(
        &config,
        handler,
        &state.trace_log_write_protection_active,
        state.crashpad_pending_stats.clone(),
    )
    .await
    {
        Ok(started) => started,
        Err(error) => {
            reclaim_initial_session_scan(state, initial_scan_task).await;
            return Err(error.to_string());
        }
    };
    if state.is_shutting_down() {
        let stop_error = runtime.stop().await.err();
        reclaim_initial_session_scan(state, initial_scan_task).await;
        return Err(stop_error.map_or_else(
            || "Codey 已进入退出流程，已取消本次 Codex 启动".to_string(),
            |error| format!("Codey 已进入退出流程，停止刚启动的 Codex 失败：{error}"),
        ));
    }
    *state.runtime.lock().await = Some(Arc::new(runtime));
    let runtime_generation = state.runtime_generation.fetch_add(1, Ordering::AcqRel) + 1;
    super::sync_wechat_claw_service(state).await;
    if let Some(initial_scan_task) = initial_scan_task {
        start_waiting_webhook_watcher(state, initial_scan_task).await;
    }
    let exit_state = Arc::clone(state);
    tokio::spawn(forward_codex_exit_to_codey_shutdown(
        exit_state,
        codex_exit,
        runtime_generation,
    ));
    Ok(json!({"status":"running"}))
}

pub(super) async fn launch_codey_inner(state: &Arc<AppState>) -> Result<Value, String> {
    ensure_runtime_can_start(state)?;
    let _operation = state.runtime_operation.lock().await;
    ensure_runtime_can_start(state)?;
    launch_codey_inner_locked(state).await
}

pub async fn launch_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    let result = launch_codey_inner(state).await;
    *state.startup_error.write().await = result.as_ref().err().cloned();
    if let Err(error) = &result {
        error_log::record_failure_with_metadata(
            "runtime_start_failed",
            "launch_codey_runtime",
            error.clone(),
            error_log::FailureMetadata {
                stage: Some("startup.runtime".to_string()),
                recoverable: Some(false),
            },
            runtime_start_failure_context(state, false, error).await,
        );
    }
    result
}

async fn runtime_start_failure_context(state: &Arc<AppState>, restart: bool, error: &str) -> Value {
    let config = state.config.read().await;
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
    json!({
        "restart": restart,
        "errorClass": classify_runtime_start_error(error),
        "codexHome": codex_home().to_string_lossy(),
        "codeyConfigPath": state.store.path().to_string_lossy(),
        "configuredCodexAppPath": config.codex_app_path.as_str(),
        "activeProfileId": active_profile.as_ref().map(|profile| profile.id.clone()),
        "activeProfileName": active_profile.as_ref().map(|profile| profile.name.clone()),
        "activeProfileOfficial": active_profile.as_ref().map(|profile| profile.official_account),
        "profileCount": config.profiles.len(),
        "officialProfileCount": official_profile_count,
        "thirdPartyProfileCount": third_party_profile_count,
        "hasThirdPartyRoute": config.has_third_party_route(),
        "routerRequiresOpenaiAuth": config.router_requires_openai_auth(),
        "officialAccountAvailable": config.official_account_available_this_launch,
        "officialAccountStatus": config.official_account_status_this_launch,
        "credentialsIncluded": false,
    })
}

fn classify_runtime_start_error(error: &str) -> &'static str {
    if error.contains("认证诊断：") || error.contains("没有可用的官方账号登录") {
        "official_auth_unavailable"
    } else if error.contains("Codex App") || error.contains("codex.exe") {
        "codex_app_unavailable"
    } else if error.contains("Provider") || error.contains("线路") {
        "provider_route_invalid"
    } else {
        "runtime_start_failed"
    }
}

pub async fn schedule_restart_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    let mut restart_task = state.restart_task.lock().await;
    ensure_runtime_can_start(state)?;
    if state.restart_in_progress.swap(true, Ordering::AcqRel) {
        return Ok(json!({"status":"already_restarting"}));
    }

    let (cancel, cancel_rx) = oneshot::channel();
    let restart_state = Arc::clone(state);
    let task = tokio::spawn(async move {
        let _restart_guard = RestartInProgressGuard {
            state: Arc::clone(&restart_state),
        };
        run_scheduled_restart(restart_state, cancel_rx).await;
    });
    *restart_task = Some(ScheduledRestart { cancel, task });

    Ok(json!({"status":"restarting"}))
}

async fn run_scheduled_restart(restart_state: Arc<AppState>, mut cancel: oneshot::Receiver<()>) {
    tokio::select! {
        // The request originates inside the Codex renderer. Let the bridge
        // deliver its response before stopping the renderer that owns it.
        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        _ = &mut cancel => return,
    }
    if restart_state.is_shutting_down() {
        return;
    }

    #[cfg(test)]
    restart_state.restart_operation_pending.notify_one();
    let _operation = tokio::select! {
        operation = restart_state.runtime_operation.lock() => operation,
        _ = &mut cancel => return,
    };
    if restart_state.is_shutting_down() {
        return;
    }

    if let Err(error) = stop_codey_runtime_locked(&restart_state).await {
        error_log::record_failure(
            "runtime_restart_failed",
            "stop_runtime_for_restart",
            error.clone(),
            json!({}),
        );
        *restart_state.startup_error.write().await = Some(error);
        return;
    }
    restart_state
        .runtime_generation
        .fetch_add(1, Ordering::AcqRel);
    if restart_state.is_shutting_down() {
        return;
    }

    let launch = launch_codey_inner_locked(&restart_state).await;
    *restart_state.startup_error.write().await = launch.as_ref().err().cloned();
    let Err(error) = launch else {
        return;
    };
    error_log::record_failure_with_metadata(
        "runtime_restart_failed",
        "launch_runtime_after_restart",
        error.clone(),
        error_log::FailureMetadata {
            stage: Some("startup.runtime".to_string()),
            recoverable: Some(false),
        },
        runtime_start_failure_context(&restart_state, true, &error).await,
    );
    eprintln!("Codey 自动重启 Codex 失败：{error}");
    restart_state.request_shutdown();
}

async fn stop_codey_runtime_locked(state: &Arc<AppState>) -> Result<Value, String> {
    super::stop_wechat_claw_service(state).await;
    stop_waiting_webhook_watcher(state).await;
    let runtime = state.runtime.lock().await.take();
    if let Some(runtime) = runtime {
        if let Err(error) = runtime.stop().await {
            *state.runtime.lock().await = Some(runtime);
            return Err(error.to_string());
        }
    } else {
        let local_router_enabled = state.config.read().await.local_router_enabled;
        restore_previous_runtime_state(codex_home(), local_router_enabled)
            .await
            .map_err(|error| error.to_string())?;
    }
    *state.startup_error.write().await = None;
    Ok(json!({"status":"stopped"}))
}

pub async fn stop_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    begin_shutdown(state).await;
    let _operation = state.runtime_operation.lock().await;
    stop_codey_runtime_locked(state).await
}

pub async fn begin_shutdown(state: &Arc<AppState>) {
    state.shutting_down.store(true, Ordering::Release);
    let restart = state.restart_task.lock().await.take();
    if let Some(ScheduledRestart { cancel, task }) = restart {
        let _ = cancel.send(());
        if let Err(error) = task.await {
            error_log::record_failure(
                "runtime_restart_failed",
                "cancel_runtime_restart_during_shutdown",
                error.to_string(),
                json!({}),
            );
        }
    }
    state.restart_in_progress.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::Ordering};
    use std::time::Duration;

    use tokio::sync::oneshot;

    use super::{
        classify_runtime_start_error, forward_codex_exit_to_codey_shutdown,
        runtime_feature_status_value, runtime_start_failure_context,
    };
    use crate::commands::{AppShutdownReason, AppState};
    use crate::config::{CodeyConfig, ProviderProfile};

    #[test]
    fn runtime_feature_status_has_a_stable_public_json_contract() {
        assert_eq!(
            runtime_feature_status_value(true, false, 2, true, false),
            serde_json::json!({
                "fastContextToolsActive": true,
                "subagentOptimizationActive": false,
                "notificationChannelsActive": true,
                "activeNotificationChannelCount": 2,
                "traceLogWriteProtectionActive": true,
                "crashpadDiskProtectionActive": false,
            })
        );
        assert_eq!(
            runtime_feature_status_value(false, true, 0, false, true)["notificationChannelsActive"],
            serde_json::Value::Bool(false)
        );
    }

    #[tokio::test]
    async fn runtime_start_failure_context_is_complete_and_does_not_include_credentials() {
        let state = Arc::new(AppState::default());
        let mut relay = ProviderProfile::new("Relay");
        relay.id = "relay".into();
        relay.base_url = "https://relay.example/v1".into();
        relay.api_key = "sk-private-runtime-key".into();
        relay.normalize();
        *state.config.write().await = CodeyConfig {
            active_profile_id: relay.id.clone(),
            profiles: vec![relay],
            ..CodeyConfig::default()
        }
        .normalize();

        let context = runtime_start_failure_context(
            &state,
            true,
            "当前 Codex 没有可用的官方账号登录。认证诊断：not logged in",
        )
        .await;
        let serialized = serde_json::to_string(&context).unwrap();

        assert_eq!(context["restart"], true);
        assert_eq!(context["errorClass"], "official_auth_unavailable");
        assert_eq!(context["activeProfileId"], "relay");
        assert_eq!(context["thirdPartyProfileCount"], 1);
        assert_eq!(context["credentialsIncluded"], false);
        assert!(context["codexHome"].is_string());
        assert!(context["codeyConfigPath"].is_string());
        assert!(!serialized.contains("sk-private-runtime-key"));
        assert!(!serialized.contains("relay.example"));
    }

    #[test]
    fn runtime_start_error_classification_is_stable() {
        assert_eq!(
            classify_runtime_start_error("没有可用的官方账号登录"),
            "official_auth_unavailable"
        );
        assert_eq!(
            classify_runtime_start_error("找不到 Codex App"),
            "codex_app_unavailable"
        );
        assert_eq!(
            classify_runtime_start_error("线路配置无效"),
            "provider_route_invalid"
        );
        assert_eq!(classify_runtime_start_error("boom"), "runtime_start_failed");
    }

    #[tokio::test]
    async fn current_codex_exit_requests_codey_shutdown() {
        let state = Arc::new(AppState::default());
        state.runtime_generation.store(7, Ordering::Release);
        let (exit_tx, exit_rx) = oneshot::channel();
        let watcher = tokio::spawn(forward_codex_exit_to_codey_shutdown(
            Arc::clone(&state),
            exit_rx,
            7,
        ));

        exit_tx.send(()).expect("signal Codex exit");
        let reason = tokio::time::timeout(Duration::from_secs(1), state.wait_for_shutdown())
            .await
            .expect("Codey shutdown was not requested after Codex exited");
        watcher.await.expect("Codex exit forwarding task failed");

        assert_eq!(reason, AppShutdownReason::CodexExited);
        assert!(state.is_shutting_down());
    }

    #[tokio::test]
    async fn stale_codex_exit_does_not_shutdown_a_new_runtime_generation() {
        let state = Arc::new(AppState::default());
        state.runtime_generation.store(8, Ordering::Release);
        let (exit_tx, exit_rx) = oneshot::channel();
        let watcher = tokio::spawn(forward_codex_exit_to_codey_shutdown(
            Arc::clone(&state),
            exit_rx,
            7,
        ));

        exit_tx.send(()).expect("signal stale Codex exit");
        watcher.await.expect("Codex exit forwarding task failed");

        assert!(!state.is_shutting_down());
        assert!(
            tokio::time::timeout(Duration::from_millis(25), state.wait_for_shutdown())
                .await
                .is_err()
        );
    }
}
