use std::path::PathBuf;
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use codey_runtime_core::app_paths::{codex_app_version, resolve_codex_app_dir_with_saved};
use serde_json::{Value, json};
use tokio::sync::oneshot;

#[cfg(windows)]
use super::ensure_windows_codex_app_path;
use super::webhooks::{
    RecentSessionScanTask, await_recent_session_scan, start_recent_session_scan,
    start_waiting_webhook_watcher, stop_waiting_webhook_watcher, webhook_watcher_should_run,
};
use super::{
    AppState, RestartInProgressGuard, ScheduledRestart, config_requires_restart,
    current_update_platform, make_bridge_handler, sync_provider_models_for_launch,
};
use crate::codex_config::codex_home;
use crate::error_log;
use crate::launcher::{CodeyRuntime, restore_previous_runtime_state, restore_runtime_config};

pub(crate) const CC_SWITCH_ROUTE_RECOVERY_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const CC_SWITCH_ROUTE_RECOVERY_STABLE_READS: u8 = 2;
const CODEX_APP_VERSION_CACHE_TTL: Duration = Duration::from_secs(30);

pub(super) struct CodexAppVersionCache {
    runtime_app_path: Option<PathBuf>,
    configured_app_path: String,
    version: String,
    lookup_started_at: Instant,
    checked_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartTrigger {
    Manual,
    RouteChange,
}

pub(crate) fn is_cc_switch_route_recovery_error(error: &str) -> bool {
    error.contains("CC Switch") || error.contains("Codex Live")
}

fn observe_route_recovery_readiness(ready_streak: &mut u8, ready: bool) -> bool {
    if ready {
        *ready_streak = ready_streak.saturating_add(1);
    } else {
        *ready_streak = 0;
    }
    *ready_streak >= CC_SWITCH_ROUTE_RECOVERY_STABLE_READS
}

pub(crate) async fn cc_switch_route_ready_for_recovery() -> bool {
    let home = codex_home();
    matches!(
        tokio::task::spawn_blocking(move || crate::cc_switch::startup_route_state(&home)).await,
        Ok(Ok(route)) if !route.takeover.managed || route.takeover.live
    )
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
    let config = state.config.read().await;
    let profile = config.active_profile();
    let configured_codex_app_path = config.codex_app_path.clone();
    let runtime_codex_app_path = runtime
        .as_ref()
        .map(|runtime| runtime.codex_app_path.clone());
    let restart_required = match (
        runtime.as_ref(),
        applied_models.as_ref(),
        applied_subagent.as_ref(),
    ) {
        (Some(runtime), Some(applied_models), Some(applied_subagent)) => config_requires_restart(
            &runtime.applied_config,
            applied_models,
            applied_subagent,
            &config,
        ),
        _ => false,
    };
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
            serde_json::to_value(update).unwrap_or(Value::Null),
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
            serde_json::to_value(&runtime.maintenance).unwrap_or_else(|_| json!({})),
        );
        let injection_statuses = if refresh_injection_status {
            runtime.refresh_injection_statuses().await
        } else {
            runtime.injection_statuses.read().await.clone()
        };
        let injection_statuses = runtime.injection_statuses_for_display(injection_statuses);
        object.insert(
            "injectionScripts".into(),
            serde_json::to_value(injection_statuses.as_ref()).unwrap_or_else(|_| json!([])),
        );
    }
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "traceLogStats".into(),
            serde_json::to_value(&state.trace_log_stats).unwrap_or_else(|_| json!({})),
        );
        object.insert(
            "crashpadPendingStats".into(),
            serde_json::to_value(&state.crashpad_pending_stats).unwrap_or_else(|_| json!({})),
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
        let configured_app_path = checked_configured_app_path.trim();
        let configured_app_path =
            (!configured_app_path.is_empty()).then(|| PathBuf::from(configured_app_path));
        let app_dir = checked_runtime_app_path.or_else(|| {
            configured_app_path
                .as_deref()
                .and_then(|path| resolve_codex_app_dir_with_saved(Some(path), None))
        });
        app_dir
            .as_deref()
            .and_then(codex_app_version)
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

pub(super) async fn refresh_injection_status(state: &Arc<AppState>) -> Result<Value, String> {
    let runtime = state.runtime.lock().await.clone();
    let Some(runtime) = runtime else {
        return Ok(json!([]));
    };
    let statuses = runtime.refresh_injection_statuses().await;
    serde_json::to_value(statuses.as_ref()).map_err(|error| error.to_string())
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

fn spawn_route_change_restart(state: Arc<AppState>, route_changed: oneshot::Receiver<()>) {
    tokio::spawn(async move {
        if route_changed.await.is_err() || state.is_shutting_down() {
            return;
        }
        eprintln!("检测到 CC Switch Live 路由变化，正在安全重启 Codex");
        if let Err(error) =
            schedule_restart_codey_runtime_with_trigger(&state, RestartTrigger::RouteChange).await
        {
            error_log::record_failure(
                "runtime_restart_failed",
                "restart_after_cc_switch_route_change",
                error.clone(),
                json!({}),
            );
            eprintln!("CC Switch 路由变化后的自动重启失败：{error}");
        }
    });
}

async fn launch_codey_inner_locked(state: &Arc<AppState>) -> Result<Value, String> {
    ensure_runtime_can_start(state)?;
    if state.runtime.lock().await.is_some() {
        return Ok(json!({"status":"already_running"}));
    }
    #[cfg(windows)]
    ensure_windows_codex_app_path(state).await?;
    stop_waiting_webhook_watcher(state).await;
    restore_previous_runtime_state(&codex_home())
        .await
        .map_err(|error| format!("恢复上次 Codey 临时 Codex 配置失败：{error}"))?;
    super::sync_cc_switch_state(state)
        .await
        .map_err(|error| format!("重新读取当前 Codex 线路失败：{error}"))?;
    let config = sync_provider_models_for_launch(state).await;
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
    let (runtime, codex_exit, route_changed) = match CodeyRuntime::start(
        &config,
        handler,
        state.crashpad_pending_stats.clone(),
        state.workflow.proxy_launch_config(),
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
    state.workflow.recover_when_ready().await;
    let runtime_generation = state.runtime_generation.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(initial_scan_task) = initial_scan_task {
        start_waiting_webhook_watcher(state, initial_scan_task).await;
    }
    if let Some(route_changed) = route_changed {
        spawn_route_change_restart(Arc::clone(state), route_changed);
    }
    let exit_state = Arc::clone(state);
    tokio::spawn(async move {
        if codex_exit.await.is_ok() {
            while exit_state.restart_in_progress.load(Ordering::Acquire) {
                let settled = exit_state.restart_settled.notified();
                if !exit_state.restart_in_progress.load(Ordering::Acquire) {
                    break;
                }
                // 兜底超时只是防丢通知，正常路径由 RestartInProgressGuard
                // 的析构即时唤醒。
                let _ = tokio::time::timeout(Duration::from_millis(250), settled).await;
            }
            if exit_state.runtime_generation.load(Ordering::Acquire) == runtime_generation {
                exit_state.request_shutdown();
            }
        }
    });
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
        let waiting_for_route_recovery = is_cc_switch_route_recovery_error(error);
        error_log::record_failure_with_metadata(
            "runtime_start_failed",
            "launch_codey_runtime",
            error.clone(),
            error_log::FailureMetadata {
                stage: Some("startup.runtime".to_string()),
                recoverable: Some(waiting_for_route_recovery),
            },
            json!({
                "restart": false,
                "waitingForRouteRecovery": waiting_for_route_recovery,
            }),
        );
    }
    result
}

pub async fn schedule_restart_codey_runtime(state: &Arc<AppState>) -> Result<Value, String> {
    schedule_restart_codey_runtime_with_trigger(state, RestartTrigger::Manual).await
}

async fn schedule_restart_codey_runtime_with_trigger(
    state: &Arc<AppState>,
    trigger: RestartTrigger,
) -> Result<Value, String> {
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
        run_scheduled_restart(restart_state, cancel_rx, trigger).await;
    });
    *restart_task = Some(ScheduledRestart { cancel, task });

    Ok(json!({"status":"restarting"}))
}

async fn wait_for_cc_switch_route_recovery(
    state: &AppState,
    cancel: &mut oneshot::Receiver<()>,
) -> bool {
    let mut ready_streak = 0;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(CC_SWITCH_ROUTE_RECOVERY_INTERVAL) => {}
            _ = &mut *cancel => return false,
        }
        if state.is_shutting_down() {
            return false;
        }
        let ready = cc_switch_route_ready_for_recovery().await;
        if observe_route_recovery_readiness(&mut ready_streak, ready) {
            return true;
        }
    }
}

async fn run_scheduled_restart(
    restart_state: Arc<AppState>,
    mut cancel: oneshot::Receiver<()>,
    trigger: RestartTrigger,
) {
    tokio::select! {
        // The request originates inside the Codex renderer. Let the bridge
        // deliver its response before stopping the renderer that owns it.
        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        _ = &mut cancel => return,
    }
    if restart_state.is_shutting_down() {
        return;
    }

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

    loop {
        let launch = launch_codey_inner_locked(&restart_state).await;
        *restart_state.startup_error.write().await = launch.as_ref().err().cloned();
        let Err(error) = launch else {
            return;
        };
        error_log::record_failure(
            "runtime_restart_failed",
            "launch_runtime_after_restart",
            error.clone(),
            json!({
                "routeChange": trigger == RestartTrigger::RouteChange,
                "waitingForRouteRecovery": is_cc_switch_route_recovery_error(&error),
            }),
        );
        eprintln!("Codey 自动重启 Codex 失败：{error}");
        if !is_cc_switch_route_recovery_error(&error) {
            restart_state.request_shutdown();
            return;
        }
        eprintln!("CC Switch 路由尚未稳定；Codey 将保持运行并等待路由恢复");
        if !wait_for_cc_switch_route_recovery(&restart_state, &mut cancel).await {
            return;
        }
        eprintln!("CC Switch 路由已稳定，正在重新启动 Codex");
    }
}

async fn stop_codey_runtime_locked(state: &Arc<AppState>) -> Result<Value, String> {
    stop_waiting_webhook_watcher(state).await;
    let runtime = state.runtime.lock().await.take();
    if let Some(runtime) = runtime {
        if let Err(error) = runtime.stop().await {
            *state.runtime.lock().await = Some(runtime);
            return Err(error.to_string());
        }
    } else {
        restore_runtime_config(&codex_home())
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
    if tokio::time::timeout(Duration::from_secs(10), state.workflow.shutdown())
        .await
        .is_err()
    {
        error_log::record_failure(
            "workflow_shutdown_timed_out",
            "pause_workflows_during_shutdown",
            "workflow shutdown exceeded ten seconds",
            json!({}),
        );
    }
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
mod route_recovery_tests {
    use super::{
        CC_SWITCH_ROUTE_RECOVERY_STABLE_READS, is_cc_switch_route_recovery_error,
        observe_route_recovery_readiness,
    };

    #[test]
    fn classifies_cc_switch_route_startup_errors_as_recoverable() {
        assert!(is_cc_switch_route_recovery_error(
            "检测到 CC Switch 已开启 Codex 路由，但当前 Live 配置未处于接管状态"
        ));
        assert!(is_cc_switch_route_recovery_error(
            "解析 Codex Live 配置失败"
        ));
        assert!(!is_cc_switch_route_recovery_error(
            "连接 Codex Renderer 失败"
        ));
    }

    #[test]
    fn route_recovery_requires_consecutive_ready_observations() {
        let mut ready_streak = 0;
        for _ in 1..CC_SWITCH_ROUTE_RECOVERY_STABLE_READS {
            assert!(!observe_route_recovery_readiness(&mut ready_streak, true));
        }
        assert!(observe_route_recovery_readiness(&mut ready_streak, true));
        assert!(!observe_route_recovery_readiness(&mut ready_streak, false));
        assert_eq!(ready_streak, 0);
    }
}
