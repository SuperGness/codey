mod account_usage;
mod cc_switch;
mod cdp;
mod codex_config;
mod codex_config_guidance;
mod codex_startup_patch;
mod commands;
mod config;
mod crashpad_pending_guard;
mod error_log;
mod fastctx_route_gate;
mod fs_util;
mod launcher;
mod maintenance_lock;
mod message_delete;
mod model_catalog;
mod model_id;
mod model_list;
mod native_update_ui;
mod notifications;
mod pending_approval;
mod pet_slim_patch;
mod plugin_marketplace;
mod process_cleanup;
mod process_tree;
mod prompt_optimization;
mod provider_lease;
mod provider_models;
mod session_delete;
mod session_index_cleanup;
mod session_metadata;
mod session_transfer;
mod sqlite_util;
mod startup_maintenance;
mod startup_update;
mod subagent_gate;
mod subagent_policy;
mod token_stats;
mod trace_log_guard;
mod trace_log_stats;
mod update_helper;

use std::path::Path;
use std::sync::Arc;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;

use commands::{AppShutdownReason, AppState};
use native_update_ui::NativeUpdateUi;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShutdownReason {
    CodexExited,
    InstallUpdate,
    Signal,
}

pub fn run_update_helper_if_requested() -> Result<bool> {
    update_helper::run_if_requested().map_err(anyhow::Error::msg)
}

pub fn run_error_log_helper_if_requested() -> Result<bool> {
    error_log::run_helper_if_requested()
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
    #[cfg(target_os = "macos")]
    {
        native_update_ui::run_macos_application(|ui| build_async_runtime()?.block_on(run(ui)))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let ui = NativeUpdateUi::start();
        let result = build_async_runtime()?.block_on(run(ui.clone()));
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

async fn run(ui: NativeUpdateUi) -> Result<()> {
    error_log::initialize();
    let state = Arc::new(AppState::default());
    let codex_home = codex_config::codex_home();
    if let Err(error) = launcher::restore_previous_runtime_state(&codex_home).await {
        error_log::record_failure_with_metadata(
            "restore_failed",
            "restore_previous_runtime_state_at_startup",
            format!("{error:#}"),
            error_log::FailureMetadata {
                stage: Some("startup.restore_previous_state".to_string()),
                recoverable: Some(true),
            },
            serde_json::json!({}),
        );
        eprintln!("Codey 启动前恢复上次临时配置失败：{error:#}");
    }
    match repair_legacy_model_catalog(&codex_home).await {
        Ok(true) => eprintln!("已修复旧版 Codey 模型目录缺失的 description 字段"),
        Ok(false) => {}
        Err(error) => {
            error_log::record_failure(
                "repair_failed",
                "repair_legacy_model_catalog",
                format!("{error:#}"),
                serde_json::json!({
                    "codexHome": codex_home,
                }),
            );
            eprintln!("修复旧版 Codey 模型目录失败：{error:#}");
        }
    }
    let mut shutdown = Box::pin(shutdown_signal());
    let startup_update = startup_update::run(&state, &ui);
    tokio::pin!(startup_update);
    let startup_update_outcome = tokio::select! {
        outcome = &mut startup_update => outcome,
        _ = &mut shutdown => return Ok(()),
    };
    if startup_update_outcome == startup_update::StartupUpdateOutcome::InstallScheduled {
        return Ok(());
    }
    let shutdown_reason = 'runtime: loop {
        match commands::launch_codey_runtime(&state).await {
            Ok(_) => {
                break tokio::select! {
                    reason = state.wait_for_shutdown() => match reason {
                        AppShutdownReason::CodexExited => ShutdownReason::CodexExited,
                        AppShutdownReason::InstallUpdate => ShutdownReason::InstallUpdate,
                    },
                    _ = &mut shutdown => ShutdownReason::Signal,
                };
            }
            Err(error) if commands::is_cc_switch_route_recovery_error(&error) => {
                eprintln!(
                    "Codey 自动启动 Codex 时检测到 CC Switch 路由尚未稳定；Codey 将保持运行并等待路由恢复：{error}"
                );
                let mut ready_streak = 0_u8;
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(
                            commands::CC_SWITCH_ROUTE_RECOVERY_INTERVAL
                        ) => {}
                        _ = &mut shutdown => break 'runtime ShutdownReason::Signal,
                    }
                    if commands::cc_switch_route_ready_for_recovery().await {
                        ready_streak = ready_streak.saturating_add(1);
                    } else {
                        ready_streak = 0;
                    }
                    if ready_streak >= commands::CC_SWITCH_ROUTE_RECOVERY_STABLE_READS {
                        eprintln!("CC Switch 路由已稳定，正在启动 Codex");
                        break;
                    }
                }
            }
            Err(error) => {
                eprintln!("Codey 自动启动 Codex 失败：{error:#}");
                let cleanup = stop_runtime_with_retry(&state).await;
                if let Err(cleanup_error) = &cleanup {
                    error_log::record_failure(
                        "restore_failed",
                        "restore_runtime_after_startup_failure",
                        cleanup_error.clone(),
                        serde_json::json!({}),
                    );
                }
                let error = initial_startup_failure_error(
                    &error,
                    cleanup.as_ref().err().map(String::as_str),
                );
                show_initial_startup_failure(&error).await;
                return Err(anyhow::Error::msg(error));
            }
        }
    };

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
    };
    match process_cleanup::terminate_other_codey_processes().await {
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
    cleanup.map_err(anyhow::Error::msg)
}

async fn repair_legacy_model_catalog(home: &Path) -> Result<bool> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || model_catalog::repair_missing_descriptions(&home))
        .await
        .map_err(anyhow::Error::from)?
}

async fn stop_runtime_with_retry(state: &Arc<AppState>) -> Result<(), String> {
    match commands::stop_codey_runtime(state).await {
        Ok(_) => Ok(()),
        Err(first_error) => {
            eprintln!("Codey 恢复 Codex 配置失败，正在重试：{first_error}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            commands::stop_codey_runtime(state)
                .await
                .map(|_| ())
                .map_err(|retry_error| format!("{first_error}；重试失败：{retry_error}"))
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

#[cfg(windows)]
async fn show_initial_startup_failure(error: &str) {
    let description = format!("{error}\n\nCodey 将退出。处理上述问题后，请重新启动 Codey。");
    if let Err(dialog_error) = tokio::task::spawn_blocking(move || {
        rfd::MessageDialog::new()
            .set_title("Codey 启动失败")
            .set_description(description)
            .set_level(rfd::MessageLevel::Error)
            .set_buttons(rfd::MessageButtons::Ok)
            .show()
    })
    .await
    {
        error_log::record_failure(
            "dialog_failed",
            "show_initial_startup_failure",
            dialog_error.to_string(),
            serde_json::json!({}),
        );
        eprintln!("Codey 启动失败提示框显示异常：{dialog_error}");
    }
}

#[cfg(not(windows))]
async fn show_initial_startup_failure(_error: &str) {}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()).context("监听 SIGTERM 失败") {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(error) => {
                eprintln!("{error:#}");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::initial_startup_failure_error;

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
