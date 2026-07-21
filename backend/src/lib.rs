mod cc_switch;
mod cdp;
mod codex_config;
mod codex_startup_patch;
mod commands;
mod config;
mod launcher;
mod maintenance_lock;
mod message_delete;
mod model_catalog;
mod pending_approval;
mod pet_slim_patch;
mod plugin_marketplace;
mod process_cleanup;
mod provider_lease;
mod provider_models;
mod session_delete;
mod session_index_cleanup;
mod session_metadata;
mod session_transfer;
mod startup_maintenance;
mod trace_log_guard;
mod trace_log_stats;
mod webhook;

use std::sync::Arc;

use anyhow::{Context, Result};

use commands::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShutdownReason {
    CodexExited,
    ManualClose,
    Signal,
}

pub async fn run() -> Result<()> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "--codey-fastctx-mcp")
    {
        hide_exclusive_windows_console();
        fastctx::cli::run_server()
            .await
            .map(|_| ())
            .map_err(anyhow::Error::msg)?;
        return Ok(());
    }

    let state = Arc::new(AppState::default());
    if let Err(error) = launcher::restore_previous_runtime_state(&codex_config::codex_home()) {
        eprintln!("Codey 启动前恢复上次临时配置失败：{error:#}");
    }
    commands::sync_cc_switch_state(&state).await;

    match commands::launch_codey_runtime(&state).await {
        Ok(_) => hide_exclusive_windows_console(),
        Err(error) => eprintln!("Codey 自动启动 Codex 失败：{error:#}"),
    }

    let shutdown_reason = tokio::select! {
        _ = state.wait_for_shutdown() => {
            if state.manual_close_requested() {
                ShutdownReason::ManualClose
            } else {
                ShutdownReason::CodexExited
            }
        },
        _ = shutdown_signal() => ShutdownReason::Signal,
    };

    let cleanup = if shutdown_reason == ShutdownReason::ManualClose {
        Ok(())
    } else {
        match commands::stop_codey_runtime(&state).await {
            Ok(_) => Ok(()),
            Err(first_error) => {
                eprintln!("Codey 恢复 Codex 配置失败，正在重试：{first_error}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                commands::stop_codey_runtime(&state)
                    .await
                    .map(|_| ())
                    .map_err(|retry_error| format!("{first_error}；重试失败：{retry_error}"))
            }
        }
    };
    if shutdown_reason == ShutdownReason::CodexExited {
        match process_cleanup::terminate_other_codey_processes().await {
            Ok(0) => {}
            Ok(count) => eprintln!("Codex 已退出，已终止 {count} 个其他 Codey 进程"),
            Err(error) => eprintln!("Codex 已退出，但清理其他 Codey 进程失败：{error:#}"),
        }
    }
    cleanup.map_err(anyhow::Error::msg)
}

fn hide_exclusive_windows_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

        // Explorer and shortcuts create a console exclusively for Codey. An
        // existing CMD/PowerShell console also contains its shell process, so
        // leave shared consoles visible instead of hiding the user's terminal.
        let mut process_ids = [0_u32; 2];
        let process_count =
            GetConsoleProcessList(process_ids.as_mut_ptr(), process_ids.len() as u32);
        if process_count == 1 {
            let console_window = GetConsoleWindow();
            if !console_window.is_null() {
                let _ = ShowWindow(console_window, SW_HIDE);
            }
        }
    }
}

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
