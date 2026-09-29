use super::*;
use std::future::Future;
use std::path::Path;

/// Only errors raised before spawn or after confirmed cleanup may use this.
#[derive(Debug)]
pub(super) struct IntegrationFailure(pub String);

impl std::fmt::Display for IntegrationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for IntegrationFailure {}

pub(super) fn recoverable(error: impl std::fmt::Display) -> anyhow::Error {
    IntegrationFailure(format!("{error:#}")).into()
}

async fn restore_then_launch<R, L>(restore: R, launch: impl FnOnce() -> L) -> Result<()>
where
    R: Future<Output = Result<()>>,
    L: Future<Output = Result<()>>,
{
    restore
        .await
        .context("原生启动前恢复配置失败，已停止自动恢复")?;
    launch().await.context("配置已恢复，但原生启动失败")
}

pub(super) async fn after_integration_failure(
    home: &Path,
    app_dir: &Path,
    local_router_enabled: bool,
    error: anyhow::Error,
) -> anyhow::Error {
    if !error.is::<IntegrationFailure>() {
        return error;
    }
    let result = restore_then_launch(
        restore_runtime_config_for_router_mode(home, local_router_enabled),
        || launch_native(app_dir, home),
    )
    .await;
    let detail = match result {
        Ok(()) => "Codey 集成未启用；配置已恢复，已提交 Codex 原生启动请求".to_string(),
        Err(recovery) => format!(
            "Codex 自动恢复失败：{recovery:#}；请保留诊断日志并通过官方入口启动或修复客户端"
        ),
    };
    error_log::record_failure(
        "integration_recovery",
        "launch_native_codex",
        format!("{error:#}；{detail}"),
        serde_json::json!({"appPath": app_dir}),
    );
    anyhow::anyhow!("{error:#}；{detail}")
}

// Do not inherit Codey's per-launch instrumentation into the native client.
fn native_command(app_dir: &Path, home: &Path) -> std::process::Command {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("/usr/bin/open");
        command
            .arg("-a")
            .arg(app_dir)
            .arg("--env")
            .arg(format!("CODEX_HOME={}", home.display()));
        command
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = std::process::Command::new(
        codey_runtime_core::app_paths::build_codex_executable(app_dir),
    );
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("CODEY_")
            || matches!(
                name.as_str(),
                "NODE_OPTIONS"
                    | "ELECTRON_RUN_AS_NODE"
                    | "CODEX_CLI_PATH"
                    | "CODEX_SPARKLE_ENABLED"
                    | "CODEX_APP_SERVER_FORCE_CLI"
            )
        {
            command.env_remove(key);
        }
    }
    // These must be removed even when absent in the parent's environment.
    command
        .env_remove("NODE_OPTIONS")
        .env_remove("CODEX_CLI_PATH")
        .env_remove("ELECTRON_RUN_AS_NODE")
        .env_remove("CODEX_APP_SERVER_FORCE_CLI")
        .env("CODEX_HOME", home);
    #[cfg(windows)]
    command.env_remove("WSL_DISTRO_NAME");
    command
}

async fn launch_native(app_dir: &Path, home: &Path) -> Result<()> {
    #[cfg(windows)]
    let refreshed = refresh_windows_packaged_app_dir(app_dir)?;
    #[cfg(windows)]
    let app_dir = refreshed.as_path();
    codey_runtime_core::app_paths::validate_codex_app_dir(app_dir)?;
    #[cfg(windows)]
    if let Some(app_id) = codey_runtime_core::app_paths::packaged_app_user_model_id(app_dir) {
        if requires_codex_home_environment(std::env::var_os("CODEX_HOME").as_deref()) {
            let spawned =
                windows_packaged::spawn_with_environment(app_dir, &native_command(app_dir, home))?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Some(status) = spawned
                .startup_process
                .as_ref()
                .context("原生启动进程句柄缺失")?
                .exit_code()?
            {
                anyhow::ensure!(status == 0, "Codex 原生启动后退出：{status}");
            }
            return Ok(());
        }
        codey_runtime_core::launcher::activate_packaged_app(&app_id, "").await?;
        return Ok(());
    }
    run_native_command(native_command(app_dir, home)).await
}

async fn run_native_command(command: std::process::Command) -> Result<()> {
    let mut command = tokio::process::Command::from(command);
    let mut child = command.spawn().context("提交 Codex 原生启动请求失败")?;
    #[cfg(target_os = "macos")]
    {
        let status = match tokio::time::timeout(Duration::from_secs(15), child.wait()).await {
            Ok(result) => result.context("等待系统应用启动器失败")?,
            Err(_) => {
                // Reap the launcher eventually without killing the GUI it may have opened.
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
                anyhow::bail!("系统应用启动器响应超时，请检查 Codex 是否已打开");
            }
        };
        anyhow::ensure!(status.success(), "系统应用启动器返回 {status}");
    }
    #[cfg(not(target_os = "macos"))]
    {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "Codex 原生启动后退出：{status}");
        } else {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoverable_failure_survives_diagnostic_context() {
        let error = recoverable("patch unavailable").context("startup attempt");
        assert!(error.is::<IntegrationFailure>());
        let cleanup_failure = anyhow::anyhow!("{error:#}; cleanup failed");
        assert!(!cleanup_failure.is::<IntegrationFailure>());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_native_launcher_reports_exit_and_spawn_failures() {
        for code in [0, 19, 0] {
            let mut command = std::process::Command::new("cmd");
            command.args(["/d", "/c", &format!("exit /b {code}")]);
            assert_eq!(run_native_command(command).await.is_ok(), code == 0);
        }
        let temp = tempfile::tempdir().unwrap();
        assert!(
            run_native_command(std::process::Command::new(temp.path().join("missing.exe")))
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_launcher_success_and_failure_are_reported() {
        // Exercise real process creation without touching the installed GUI.
        for _ in 0..2 {
            run_native_command(std::process::Command::new("/usr/bin/true"))
                .await
                .unwrap();
        }
        assert!(
            run_native_command(std::process::Command::new("/usr/bin/false"))
                .await
                .is_err()
        );
        assert!(
            run_native_command(std::process::Command::new("/missing/codex-launcher"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn native_launch_runs_only_after_successful_restore() {
        let calls = std::cell::RefCell::new(Vec::new());
        restore_then_launch(
            async {
                calls.borrow_mut().push("restore");
                Ok(())
            },
            || async {
                calls.borrow_mut().push("launch");
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(*calls.borrow(), ["restore", "launch"]);
        calls.borrow_mut().clear();
        let error = restore_then_launch(async { anyhow::bail!("permission denied") }, || async {
            calls.borrow_mut().push("launch");
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(calls.borrow().is_empty());
        assert!(format!("{error:#}").contains("permission denied"));
    }

    #[tokio::test]
    async fn cleanup_errors_do_not_trigger_native_recovery() {
        let error = after_integration_failure(
            Path::new("missing"),
            Path::new("missing"),
            false,
            anyhow::anyhow!("cleanup failed"),
        )
        .await;
        assert_eq!(error.to_string(), "cleanup failed");
    }

    #[tokio::test]
    async fn native_launch_failure_remains_diagnostic() {
        let error = restore_then_launch(async { Ok(()) }, || async {
            anyhow::bail!("client file locked")
        })
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("client file locked"));
    }

    #[test]
    fn native_command_has_no_debug_or_wrapper_arguments() {
        let command = native_command(Path::new("/test/Codex.app"), Path::new("/test/home"));
        for arg in command.get_args() {
            let arg = arg.to_string_lossy();
            assert!(
                !arg.contains("inspect")
                    && !arg.contains("remote-debugging")
                    && !arg.contains("require")
            );
        }
        for key in ["NODE_OPTIONS", "CODEX_CLI_PATH", "ELECTRON_RUN_AS_NODE"] {
            assert!(
                command
                    .get_envs()
                    .any(|(name, value)| name == key && value.is_none())
            );
        }
    }
}
