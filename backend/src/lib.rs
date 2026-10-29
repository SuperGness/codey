mod account_usage;
mod appserver_call;
mod cdp;
mod codex_config;
mod codex_config_guidance;
mod codex_extensions;
mod codex_provider;
mod codex_startup_patch;
mod codey_plugins;
mod commands;
mod computer_use;
mod config;
mod conversation_stats;
mod crashpad_pending_guard;
#[cfg(windows)]
mod desktop_instance;
mod error_log;
pub mod fastctx;
mod fastctx_route_gate;
mod fs_util;
mod hook_io;
mod http_response;
mod launch_proxy;
mod launch_proxy_relay;
mod launcher;
mod local_router;
mod maintenance_lock;
mod message_delete;
mod model_catalog;
mod model_id;
mod model_list;
mod native_update_ui;
mod notifications;
mod official_accounts;
mod pending_approval;
mod pet_slim_patch;
mod plugin_log_terminal;
mod plugin_marketplace;
mod plugin_routes;
mod process_cleanup;
mod process_tree;
mod prompt_optimization;
mod provider_models;
mod renderer_asset_patch;
mod route_request_log;
mod session_index_cleanup;
mod session_metadata;
mod session_model;
mod session_transfer;
mod sqlite_util;
mod startup_update;
mod subagent;
mod subagent_gate;
mod subagent_orchestrator;
mod subagent_policy;
mod target_prefetch;
mod task_activity;
mod trace_log_guard;
mod trace_log_stats;
mod update_helper;
#[cfg(any(windows, test))]
mod windows_session;

// 单元测试不能写入用户真实的 Codey 状态目录：诊断日志和错误日志都按该目录
// 定位，否则测试记录会淹没真实故障并提前触发日志轮转。在 main 之前设置，
// 保证任何测试线程读取路径时都已生效。`cargo run` 和 `cargo test` 重建的
// 正式二进制都不包含这段代码。
#[cfg(test)]
mod test_state_isolation {
    extern "C" fn isolate_app_state_dir() {
        const KEY: &str = "CODEY_APP_STATE_DIR";
        if std::env::var_os(KEY).is_some_and(|value| !value.is_empty()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("codey-test-state-{}", std::process::id()));
        // SAFETY: runs as a pre-main initializer before any other thread exists.
        unsafe { std::env::set_var(KEY, dir) };
    }

    #[used]
    #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
    #[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
    #[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
    static ISOLATE_APP_STATE_DIR: extern "C" fn() = isolate_app_state_dir;
}

use std::sync::Arc;

use anyhow::{Context, Result};

use commands::{AppShutdownReason, AppState};
use native_update_ui::NativeUpdateUi;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShutdownReason {
    CodexExited,
    InstallUpdate,
    Signal,
    RuntimeRestartFailed,
}

impl ShutdownReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::CodexExited => "codex_exited",
            Self::InstallUpdate => "install_update",
            Self::Signal => "signal",
            Self::RuntimeRestartFailed => "runtime_restart_failed",
        }
    }
}

struct DesktopLifecycle {
    started_at: std::time::Instant,
    reason: std::sync::Mutex<&'static str>,
}

impl DesktopLifecycle {
    fn new() -> Self {
        Self {
            started_at: std::time::Instant::now(),
            reason: std::sync::Mutex::new("startup_failed"),
        }
    }

    fn set_reason(&self, reason: &'static str) {
        *self
            .reason
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = reason;
    }

    fn shutdown_requested(&self, reason: ShutdownReason) {
        self.set_reason(reason.as_str());
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "desktop.shutdown_requested",
            serde_json::json!({
                "processId": std::process::id(),
                "uptimeMs": self.started_at.elapsed().as_millis(),
                "reason": reason.as_str(),
            }),
        );
        let _ = codey_runtime_core::diagnostic_log::flush_diagnostic_log();
    }

    fn exit_details(&self, success: bool, panicking: bool) -> serde_json::Value {
        let reason = *self
            .reason
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        serde_json::json!({
            "processId": std::process::id(),
            "uptimeMs": self.started_at.elapsed().as_millis(),
            "reason": if panicking { "panic" } else { reason },
            "success": success && !panicking,
            "exitCode": if panicking { None } else { Some(if success { 0 } else { 1 }) },
        })
    }
}

struct DesktopExitGuard {
    lifecycle: Arc<DesktopLifecycle>,
    success: bool,
}

impl Drop for DesktopExitGuard {
    fn drop(&mut self) {
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "desktop.finished",
            self.lifecycle
                .exit_details(self.success, std::thread::panicking()),
        );
        let _ = codey_runtime_core::diagnostic_log::flush_diagnostic_log();
    }
}

pub fn run_update_helper_if_requested() -> Result<bool> {
    update_helper::run_if_requested().map_err(anyhow::Error::msg)
}

pub fn run_error_log_helper_if_requested() -> Result<bool> {
    error_log::run_helper_if_requested()
}

pub fn run_plugin_log_terminal_if_requested() -> Result<bool> {
    plugin_log_terminal::run_if_requested()
}

pub fn run_codex_cli_wrapper_if_requested() -> Result<bool> {
    codex_startup_patch::run_cli_wrapper_if_requested()
}

pub fn install_crash_log_hook(component: &'static str, stage: &'static str) {
    error_log::install_panic_hook(component, stage);
}

pub fn record_process_failure(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    stage: impl Into<String>,
) {
    error_log::record_process_failure(event, operation, error, stage);
}

pub fn record_process_failure_with_recoverability(
    event: impl Into<String>,
    operation: impl Into<String>,
    error: impl Into<String>,
    stage: impl Into<String>,
    recoverable: bool,
) {
    error_log::record_process_failure_with_recoverability(
        event,
        operation,
        error,
        stage,
        recoverable,
    );
}

pub fn run_subagent_gate_hook_if_requested() -> Result<bool> {
    subagent_gate::run_hook_if_requested()
}

pub fn run_fastctx_route_hook_if_requested() -> Result<bool> {
    fastctx_route_gate::run_hook_if_requested()
}

pub fn run_desktop_application() -> Result<()> {
    let lifecycle = Arc::new(DesktopLifecycle::new());
    let mut exit_guard = DesktopExitGuard {
        lifecycle: Arc::clone(&lifecycle),
        success: false,
    };
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "desktop.started",
        serde_json::json!({ "processId": std::process::id() }),
    );
    let result = run_desktop_application_inner(lifecycle);
    exit_guard.success = result.is_ok();
    result
}

fn run_desktop_application_inner(lifecycle: Arc<DesktopLifecycle>) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        native_update_ui::run_macos_application(move |ui| {
            build_async_runtime()?.block_on(run(ui, lifecycle))
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        #[cfg(windows)]
        let Some(_desktop_instance) = desktop_instance::claim() else {
            lifecycle.set_reason("instance_not_acquired");
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "desktop.instance_not_acquired",
                serde_json::json!({}),
            );
            return Ok(());
        };
        let ui = NativeUpdateUi::start();
        let result = build_async_runtime()?.block_on(run(ui.clone(), lifecycle));
        ui.shutdown();
        result
    }
}

fn build_async_runtime() -> Result<tokio::runtime::Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    // Codey is an I/O coordinator. Blocking filesystem/SQLite work already
    // runs on Tokio's blocking pool, so two async workers avoid creating a
    // CPU-count-sized thread team for every helper instance.
    builder.worker_threads(2);
    builder.enable_all().build().map_err(anyhow::Error::from)
}

struct PluginShutdownGuard;

impl Drop for PluginShutdownGuard {
    fn drop(&mut self) {
        codey_plugins::shutdown();
    }
}

async fn run(ui: NativeUpdateUi, lifecycle: Arc<DesktopLifecycle>) -> Result<()> {
    // Config load, ledger read and HTTP client construction (which loads the
    // system root store) are synchronous; keep them off the async workers.
    let state = tokio::task::spawn_blocking(|| {
        error_log::initialize();
        let state = Arc::new(AppState::default());
        let configured_codex_app_path = state.config.blocking_read().codex_app_path.clone();
        error_log::refresh_codex_app_version(None, Some(&configured_codex_app_path));
        commands::install_plugin_route_handler(Arc::clone(&state));
        let plugin_root = codey_runtime_core::paths::default_app_state_dir().join("codey-plugins");
        if let Err(error) = codey_plugins::initialize(plugin_root) {
            error_log::record_failure(
                "plugin_initialization_failed",
                "initialize_codey_plugins",
                error,
                serde_json::json!({}),
            );
        }
        state
    })
    .await
    .context("初始化 Codey 状态的任务异常退出")?;
    let _plugin_shutdown = PluginShutdownGuard;
    // 临时状态的恢复和路由兼容桩都在每次启动里做。进程入口再做一次的话，
    // 第一次失败只记日志，第二次同样的错误才会中止启动。
    // Resolve the default official account while the launch path prepares its
    // remaining state. Update checks start only after the runtime is ready.
    state.prewarm_official_account_probe().await;
    // Register signal listeners during launch even though the update workflow
    // no longer polls them before launching Codex.
    let shutdown_task = tokio::spawn(shutdown_signal());
    let shutdown = wait_for_shutdown_signal_task(shutdown_task);
    tokio::pin!(shutdown);
    let show_initial_startup_failure = |error: &str| {
        let ui = ui.clone();
        let error = error.to_owned();
        async move { show_initial_startup_failure_with_ui(&ui, &error).await }
    };
    let shutdown_reason = loop {
        match commands::launch_codey_runtime(&state).await {
            Ok(_) => {
                lifecycle.set_reason("runtime_failed");
                break wait_for_runtime_shutdown(
                    startup_update::run(&state, &ui),
                    state.wait_for_shutdown(),
                    &mut shutdown,
                )
                .await;
            }
            Err(error) => {
                eprintln!("Codey 自动启动 Codex 失败：{error:#}");
                let context_recovery = error.is::<model_catalog::ContextBudgetCatalogError>();
                let mut error = format!("{error:#}");
                let cleanup = if context_recovery {
                    commands::cleanup_failed_runtime_start(&state)
                        .await
                        .map(|_| ())
                } else {
                    stop_runtime_with_retry(&state).await
                };
                if let Err(cleanup_error) = &cleanup {
                    error_log::record_failure(
                        "restore_failed",
                        "restore_runtime_after_startup_failure",
                        cleanup_error.clone(),
                        serde_json::json!({}),
                    );
                }
                if cleanup.is_ok() && context_recovery {
                    match commands::recover_default_context_budgets_for_launch(&state, &error).await
                    {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(recovery_error) => {
                            error = format!("{error}；恢复默认上下文预算失败：{recovery_error}");
                        }
                    }
                }
                let result = finish_failed_startup(
                    &error,
                    cleanup,
                    startup_update::run_after_launch_failure(&state, &ui),
                    &mut shutdown,
                )
                .await;
                if let Err(error) = &result {
                    show_initial_startup_failure(error).await;
                }
                return result
                    .map(|reason| lifecycle.shutdown_requested(reason))
                    .map_err(anyhow::Error::msg);
            }
        }
    };

    let shutdown_started_at = std::time::SystemTime::now();
    lifecycle.shutdown_requested(shutdown_reason);
    let cleanup = stop_runtime_with_retry(&state).await;
    if let Err(error) = &cleanup {
        error_log::record_failure(
            "restore_failed",
            "restore_runtime_during_shutdown",
            error.clone(),
            serde_json::json!({}),
        );
    }
    let shutdown_context = match shutdown_reason {
        ShutdownReason::CodexExited => "Codex 已退出",
        ShutdownReason::InstallUpdate => "Codey 正在安装更新",
        ShutdownReason::Signal => "Codey 收到退出信号",
        ShutdownReason::RuntimeRestartFailed => "Codey 自动重启 Codex 失败",
    };
    match process_cleanup::terminate_other_codey_processes_since(shutdown_started_at).await {
        Ok(0) => {}
        Ok(count) => eprintln!("{shutdown_context}，已终止 {count} 个遗留 Codey 进程"),
        Err(error) => {
            error_log::record_failure(
                "cleanup_failed",
                "terminate_other_codey_processes",
                format!("{error:#}"),
                serde_json::json!({
                    "shutdownContext": shutdown_context,
                }),
            );
            eprintln!("{shutdown_context}，但清理遗留 Codey 进程失败：{error:#}");
        }
    }
    runtime_shutdown_result(shutdown_reason, cleanup).map_err(anyhow::Error::msg)
}

fn runtime_shutdown_result(
    reason: ShutdownReason,
    cleanup: Result<(), String>,
) -> Result<(), String> {
    match (reason, cleanup) {
        (ShutdownReason::RuntimeRestartFailed, Ok(())) => Err("自动重启 Codex 失败".to_string()),
        (ShutdownReason::RuntimeRestartFailed, Err(error)) => {
            Err(format!("自动重启 Codex 失败；退出清理失败：{error}"))
        }
        (_, result) => result,
    }
}

async fn finish_failed_startup(
    startup_error: &str,
    cleanup: Result<(), String>,
    update: impl std::future::Future<Output = startup_update::StartupUpdateOutcome>,
    signal: impl std::future::Future<Output = ()>,
) -> Result<ShutdownReason, String> {
    // Only offer replacement after Codex has stopped and its temporary state
    // has been restored. A failed cleanup must preserve its original error.
    if cleanup.is_ok() {
        tokio::select! {
            biased;
            _ = signal => return Ok(ShutdownReason::Signal),
            outcome = update => {
                if outcome == startup_update::StartupUpdateOutcome::InstallScheduled {
                    return Ok(ShutdownReason::InstallUpdate);
                }
            }
        }
    }
    Err(initial_startup_failure_error(
        startup_error,
        cleanup.as_ref().err().map(String::as_str),
    ))
}

async fn wait_for_runtime_shutdown(
    update: impl std::future::Future<Output = startup_update::StartupUpdateOutcome>,
    app_shutdown: impl std::future::Future<Output = AppShutdownReason>,
    signal: impl std::future::Future<Output = ()>,
) -> ShutdownReason {
    tokio::pin!(update, app_shutdown, signal);
    let mut update_pending = true;
    loop {
        tokio::select! {
            biased;
            reason = &mut app_shutdown => return match reason {
                AppShutdownReason::CodexExited => ShutdownReason::CodexExited,
                AppShutdownReason::InstallUpdate => ShutdownReason::InstallUpdate,
                AppShutdownReason::RuntimeRestartFailed => ShutdownReason::RuntimeRestartFailed,
            },
            _ = &mut signal => return ShutdownReason::Signal,
            outcome = &mut update, if update_pending => {
                update_pending = false;
                if outcome == startup_update::StartupUpdateOutcome::InstallScheduled {
                    // The runtime is already running; installing must use the
                    // same cleanup path as an update from the console.
                    return ShutdownReason::InstallUpdate;
                }
            }
        }
    }
}

async fn stop_runtime_with_retry(state: &Arc<AppState>) -> Result<(), String> {
    match commands::stop_codey_runtime(state).await {
        Ok(_) => Ok(()),
        Err(first_error) => {
            eprintln!("Codey 停止 Codex 或恢复配置失败，正在重试：{first_error}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let retry_error = match commands::stop_codey_runtime(state).await {
                Ok(_) => return Ok(()),
                Err(error) => error,
            };
            let error = format!("{first_error}；重试失败：{retry_error}");
            match commands::reap_runtime_child_before_exit(state).await {
                Ok(()) => Err(error),
                Err(reap_error) => Err(format!("{error}；最终回收直属子进程失败：{reap_error}")),
            }
        }
    }
}

fn initial_startup_failure_error(startup_error: &str, cleanup_error: Option<&str>) -> String {
    match cleanup_error {
        Some(cleanup_error) => {
            format!("{startup_error}；启动失败后的清理也失败：{cleanup_error}")
        }
        None => startup_error.to_string(),
    }
}

// NativeUpdateUi renders the fatal dialog with rfd::MessageDialog::new(),
// MessageLevel::Error, MessageButtons::Ok and .show() inside
// tokio::task::spawn_blocking, using .set_title("Codey 启动失败") with the
// message "Codey 将退出。处理上述问题后，请重新启动 Codey。".
async fn show_initial_startup_failure_with_ui(ui: &NativeUpdateUi, error: &str) {
    if let Err(dialog_error) = ui.show_startup_failure(error).await {
        error_log::record_failure(
            "dialog_failed",
            "show_initial_startup_failure",
            dialog_error.to_string(),
            serde_json::json!({}),
        );
        eprintln!("Codey 启动失败提示框显示异常：{dialog_error}");
    }
}

async fn wait_for_shutdown_signal_task(task: tokio::task::JoinHandle<()>) {
    if let Err(error) = task.await {
        error_log::record_failure(
            "shutdown_signal_listener_failed",
            "wait_for_shutdown_signal_task",
            error.to_string(),
            serde_json::json!({
                "cancelled": error.is_cancelled(),
                "panicked": error.is_panic(),
            }),
        );
        // 监听任务异常不代表收到退出信号，继续等待 Codex 退出或安装更新。
        std::future::pending::<()>().await;
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()).context("监听 SIGTERM 失败") {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = ctrl_c_signal() => {}
                    _ = wait_for_termination_signal(terminate.recv()) => {}
                }
            }
            Err(error) => {
                error_log::record_failure(
                    "shutdown_signal_listener_failed",
                    "register_sigterm_listener",
                    format!("{error:#}"),
                    serde_json::json!({}),
                );
                eprintln!("{error:#}");
                ctrl_c_signal().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c_signal().await;
    }
}

#[cfg(any(unix, test))]
async fn wait_for_termination_signal(signal: impl std::future::Future<Output = Option<()>>) {
    if signal.await.is_none() {
        error_log::record_failure(
            "shutdown_signal_listener_failed",
            "wait_for_sigterm",
            "SIGTERM 监听流已关闭，未收到退出信号",
            serde_json::json!({}),
        );
        std::future::pending::<()>().await;
    }
}

/// A listener that cannot be registered never fires, so Codex exit remains
/// the shutdown trigger instead of Codey quitting right after launch.
async fn ctrl_c_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error_log::record_failure(
            "shutdown_signal_listener_failed",
            "wait_for_ctrl_c",
            error.to_string(),
            serde_json::json!({}),
        );
        eprintln!("监听 Ctrl+C 失败：{error}");
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use std::{future::pending, time::Duration};

    use super::*;

    #[tokio::test]
    async fn failed_startup_keeps_error_when_no_update_is_installed() {
        assert_eq!(
            finish_failed_startup(
                "Codex 启动失败",
                Ok(()),
                async { startup_update::StartupUpdateOutcome::Continue },
                pending(),
            )
            .await,
            Err("Codex 启动失败".to_string())
        );
    }

    #[tokio::test]
    async fn failed_startup_can_install_update_after_cleanup() {
        assert_eq!(
            finish_failed_startup(
                "Codex 启动失败",
                Ok(()),
                async { startup_update::StartupUpdateOutcome::InstallScheduled },
                pending(),
            )
            .await,
            Ok(ShutdownReason::InstallUpdate)
        );
    }

    #[tokio::test]
    async fn failed_startup_does_not_poll_update_when_cleanup_failed() {
        assert_eq!(
            finish_failed_startup(
                "Codex 启动失败",
                Err("配置恢复失败".to_string()),
                async { panic!("清理失败后不应检查或安装更新") },
                pending(),
            )
            .await,
            Err("Codex 启动失败；启动失败后的清理也失败：配置恢复失败".to_string())
        );
    }

    #[tokio::test]
    async fn failed_startup_update_can_be_cancelled_by_shutdown() {
        assert_eq!(
            finish_failed_startup("Codex 启动失败", Ok(()), pending(), async {}).await,
            Ok(ShutdownReason::Signal)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn slow_update_check_does_not_delay_codex_exit() {
        let started = tokio::time::Instant::now();
        let reason = wait_for_runtime_shutdown(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                panic!("更新检查应随 Codex 退出而取消");
            },
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                AppShutdownReason::CodexExited
            },
            pending(),
        )
        .await;
        assert_eq!(reason, ShutdownReason::CodexExited);
        assert_eq!(started.elapsed(), Duration::from_millis(20));
    }

    #[tokio::test]
    async fn shutdown_signal_cancels_pending_update() {
        assert_eq!(
            wait_for_runtime_shutdown(pending(), pending(), async {}).await,
            ShutdownReason::Signal
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failed_signal_listener_does_not_trigger_shutdown() {
        let listener = tokio::spawn(async { panic!("signal listener failed") });
        let reason = wait_for_runtime_shutdown(
            pending(),
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                AppShutdownReason::CodexExited
            },
            wait_for_shutdown_signal_task(listener),
        )
        .await;
        assert_eq!(reason, ShutdownReason::CodexExited);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_signal_listener_does_not_trigger_shutdown() {
        let listener = tokio::spawn(pending::<()>());
        listener.abort();
        assert!(
            tokio::time::timeout(
                Duration::from_secs(1),
                wait_for_shutdown_signal_task(listener),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn completed_signal_listener_triggers_shutdown() {
        let reason = wait_for_runtime_shutdown(
            pending(),
            pending(),
            wait_for_shutdown_signal_task(tokio::spawn(async {})),
        )
        .await;
        assert_eq!(reason, ShutdownReason::Signal);
    }

    #[tokio::test(start_paused = true)]
    async fn closed_sigterm_stream_does_not_trigger_shutdown() {
        let reason = wait_for_runtime_shutdown(
            pending(),
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                AppShutdownReason::InstallUpdate
            },
            wait_for_termination_signal(async { None }),
        )
        .await;
        assert_eq!(reason, ShutdownReason::InstallUpdate);
    }

    #[tokio::test]
    async fn received_sigterm_triggers_shutdown() {
        let reason = wait_for_runtime_shutdown(
            pending(),
            pending(),
            wait_for_termination_signal(async { Some(()) }),
        )
        .await;
        assert_eq!(reason, ShutdownReason::Signal);
    }

    #[test]
    fn desktop_exit_diagnostics_preserve_reason_and_cleanup_failure() {
        let lifecycle = DesktopLifecycle::new();
        for reason in [
            ShutdownReason::CodexExited,
            ShutdownReason::InstallUpdate,
            ShutdownReason::Signal,
        ] {
            lifecycle.set_reason(reason.as_str());
            for success in [true, false] {
                let details = lifecycle.exit_details(success, false);
                assert_eq!(details["processId"], std::process::id());
                assert_eq!(details["reason"], reason.as_str());
                assert_eq!(details["success"], success);
                assert_eq!(details["exitCode"], if success { 0 } else { 1 });
                assert!(details["uptimeMs"].is_u64());
            }
        }
        lifecycle.set_reason("instance_not_acquired");
        assert_eq!(
            lifecycle.exit_details(true, false)["reason"],
            "instance_not_acquired"
        );
        let panic = lifecycle.exit_details(false, true);
        assert_eq!(panic["reason"], "panic");
        assert_eq!(panic["success"], false);
        assert!(panic["exitCode"].is_null());
        assert_eq!(
            DesktopLifecycle::new().exit_details(false, false)["reason"],
            "startup_failed"
        );
    }

    #[tokio::test]
    async fn runtime_restart_failure_remains_failure_after_shutdown_cleanup() {
        let reason = wait_for_runtime_shutdown(
            pending(),
            async { AppShutdownReason::RuntimeRestartFailed },
            pending(),
        )
        .await;
        assert_eq!(reason, ShutdownReason::RuntimeRestartFailed);
        let lifecycle = DesktopLifecycle::new();
        lifecycle.set_reason(reason.as_str());
        for cleanup in [Ok(()), Err("配置恢复失败".to_string())] {
            let result = runtime_shutdown_result(reason, cleanup.clone());
            let details = lifecycle.exit_details(result.is_ok(), false);
            assert_eq!(details["reason"], "runtime_restart_failed");
            assert_eq!(details["success"], false);
            assert_eq!(details["exitCode"], 1);
            let error = result.unwrap_err();
            assert!(error.contains("自动重启 Codex 失败"));
            if let Err(cleanup_error) = cleanup {
                assert!(error.contains(&cleanup_error));
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn completed_update_check_keeps_runtime_alive_until_shutdown() {
        let started = tokio::time::Instant::now();
        let reason = wait_for_runtime_shutdown(
            async { startup_update::StartupUpdateOutcome::Continue },
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                AppShutdownReason::InstallUpdate
            },
            pending(),
        )
        .await;
        assert_eq!(reason, ShutdownReason::InstallUpdate);
        assert_eq!(started.elapsed(), Duration::from_secs(1));
    }

    #[tokio::test]
    async fn confirmed_background_update_uses_runtime_shutdown() {
        assert_eq!(
            wait_for_runtime_shutdown(
                async { startup_update::StartupUpdateOutcome::InstallScheduled },
                pending(),
                pending(),
            )
            .await,
            ShutdownReason::InstallUpdate
        );
    }

    #[tokio::test]
    async fn ready_shutdown_prevents_starting_an_update() {
        assert_eq!(
            wait_for_runtime_shutdown(
                async { panic!("退出时不应继续检查或安装更新") },
                async { AppShutdownReason::CodexExited },
                pending(),
            )
            .await,
            ShutdownReason::CodexExited
        );
    }

    #[test]
    fn startup_failure_keeps_the_cleanup_error() {
        assert_eq!(
            initial_startup_failure_error("Codex 启动失败", Some("配置恢复失败")),
            "Codex 启动失败；启动失败后的清理也失败：配置恢复失败"
        );
    }

    #[test]
    fn startup_failure_is_unchanged_after_successful_cleanup() {
        assert_eq!(
            initial_startup_failure_error("Codex 启动失败", None),
            "Codex 启动失败"
        );
    }
}
