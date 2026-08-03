mod account_usage;
mod cc_switch;
mod cdp;
mod codex_config;
mod codex_startup_patch;
mod commands;
mod config;
mod error_log;
mod fs_util;
mod launcher;
mod maintenance_lock;
mod message_delete;
mod model_catalog;
mod notifications;
mod pending_approval;
mod pet_slim_patch;
mod plugin_marketplace;
mod process_cleanup;
mod process_tree;
mod provider_lease;
mod provider_models;
mod session_delete;
mod session_index_cleanup;
mod session_metadata;
mod session_transfer;
mod sqlite_util;
mod startup_maintenance;
mod trace_log_guard;
mod trace_log_stats;
mod update_helper;

use std::sync::Arc;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;

use commands::{AppShutdownReason, AppState};

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

pub async fn run() -> Result<()> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "--codey-fastctx-mcp")
    {
        fastctx::cli::run_server()
            .await
            .map(|_| ())
            .map_err(anyhow::Error::msg)?;
        return Ok(());
    }

    error_log::initialize();
    let state = Arc::new(AppState::default());
    let restore_started_at = std::time::Instant::now();
    if let Err(error) = launcher::restore_previous_runtime_state(&codex_config::codex_home()) {
        error_log::record_failure_with_metadata(
            "restore_failed",
            "restore_previous_runtime_state_at_startup",
            format!("{error:#}"),
            error_log::FailureMetadata {
                stage: Some("startup.restore_previous_state".to_string()),
                duration_ms: Some(
                    u64::try_from(restore_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                ),
                attempts: Some(1),
                timeout_ms: None,
                recoverable: Some(true),
            },
            serde_json::json!({}),
        );
        eprintln!("Codey 启动前恢复上次临时配置失败：{error:#}");
    }
    commands::sync_cc_switch_state(&state).await;

    if let Err(error) = commands::launch_codey_runtime(&state).await {
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
        let error =
            initial_startup_failure_error(&error, cleanup.as_ref().err().map(String::as_str));
        show_initial_startup_failure(&error).await;
        return Err(anyhow::Error::msg(error));
    }

    let shutdown_reason = tokio::select! {
        reason = state.wait_for_shutdown() => match reason {
            AppShutdownReason::CodexExited => ShutdownReason::CodexExited,
            AppShutdownReason::InstallUpdate => ShutdownReason::InstallUpdate,
        },
        _ = shutdown_signal() => ShutdownReason::Signal,
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
