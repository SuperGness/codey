use super::*;

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildProcessState {
    Running,
    Exited,
    Untracked,
}

#[cfg(windows)]
async fn child_process_state(child: &Arc<Mutex<Option<Child>>>) -> ChildProcessState {
    let mut slot = child.lock().await;
    let state = match slot.as_mut() {
        Some(process) => match process.try_wait() {
            Ok(Some(_)) => ChildProcessState::Exited,
            Ok(None) => ChildProcessState::Running,
            Err(_) => ChildProcessState::Running,
        },
        None => ChildProcessState::Untracked,
    };
    if state == ChildProcessState::Exited {
        slot.take();
    }
    state
}

#[cfg(not(windows))]
pub(super) fn spawn_codex_exit_watcher(
    child: Arc<Mutex<Option<Child>>>,
    codex_exited: Arc<AtomicBool>,
) -> (
    oneshot::Sender<()>,
    oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let Some(mut process) = child.lock().await.take() else {
            return;
        };
        let wait_result = tokio::select! {
            _ = &mut shutdown_rx => None,
            result = process.wait() => Some(result),
        };
        let natural_exit = match wait_result {
            Some(Ok(_)) => true,
            Some(Err(error)) => {
                error_log::record_failure(
                    "process_watch_failed",
                    "wait_for_codex_exit",
                    error.to_string(),
                    serde_json::json!({
                        "processId": process.id(),
                    }),
                );
                *child.lock().await = Some(process);
                false
            }
            None => {
                *child.lock().await = Some(process);
                false
            }
        };
        if natural_exit {
            codex_exited.store(true, Ordering::Release);
            let _ = exit_tx.send(());
        }
    });
    (shutdown_tx, exit_rx, task)
}

#[cfg(windows)]
pub(super) fn spawn_codex_exit_watcher(
    child: Arc<Mutex<Option<Child>>>,
    process_id: Option<u32>,
    codex_exited: Arc<AtomicBool>,
) -> (
    oneshot::Sender<()>,
    oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let natural_exit = if let Some(process_id) = process_id {
            tokio::select! {
                _ = &mut shutdown_rx => false,
                result = codey_runtime_core::launcher::wait_for_windows_process_id(process_id) => {
                    match result {
                        Ok(()) => true,
                        Err(error) => {
                            error_log::record_failure(
                                "process_watch_failed",
                                "wait_for_windows_codex_exit",
                                format!("{error:#}"),
                                serde_json::json!({
                                    "processId": process_id,
                                }),
                            );
                            eprintln!("等待 Windows Codex 进程退出失败：{error:#}");
                            !codey_runtime_core::windows_enumerate_processes()
                                .iter()
                                .any(|process| process.process_id == process_id)
                        }
                    }
                }
            }
        } else {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break false,
                    _ = interval.tick() => match child_process_state(&child).await {
                        ChildProcessState::Running => {}
                        ChildProcessState::Exited => break true,
                        ChildProcessState::Untracked => break false,
                    }
                }
            }
        };
        if natural_exit {
            codex_exited.store(true, Ordering::Release);
            let _ = exit_tx.send(());
        }
    });
    (shutdown_tx, exit_rx, task)
}

pub(super) struct SpawnedCodex {
    pub(super) child: Option<Child>,
    pub(super) process_id: Option<u32>,
    #[cfg(unix)]
    pub(super) process_group_id: Option<u32>,
    #[cfg(target_os = "macos")]
    pub(super) inspector_argument: Option<String>,
    pub(super) performance_status: String,
    pub(super) performance_detail: String,
}

// Launch inputs intentionally mirror the independently validated runtime
// settings; keeping them explicit prevents a partially populated option bag.
#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_codex(
    app_dir: &std::path::Path,
    debug_port: u16,
    disable_codex_pet: bool,
    fast_codex_startup: bool,
    subagent_gate_active: bool,
    gpu_launch_mode: GpuLaunchMode,
    runtime_config_overrides: &[String],
    workflow_proxy: Option<&crate::codex_startup_patch::WorkflowProxyLaunchConfig>,
) -> Result<SpawnedCodex> {
    #[cfg(any(windows, target_os = "macos"))]
    let patch_options = crate::codex_startup_patch::PatchOptions {
        disable_pet: disable_codex_pet,
        fast_codex_startup,
        subagent_gate_active,
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = (
        disable_codex_pet,
        fast_codex_startup,
        subagent_gate_active,
        runtime_config_overrides,
        workflow_proxy,
    );
    let runtime_arguments = codex_runtime_arguments(gpu_launch_mode, !cfg!(target_os = "macos"));

    #[cfg(windows)]
    {
        let inspector_port =
            crate::codex_startup_patch::reserve_loopback_port().map_err(|error| {
                let error = error.context("为 Codex 启动补丁选择本地调试端口失败");
                error_log::record_failure(
                    "patch_failed",
                    "reserve_startup_patch_port",
                    format!("{error:#}"),
                    serde_json::json!({
                        "platform": "windows",
                    }),
                );
                error
            })?;
        let inspector_arg = crate::codex_startup_patch::inspector_argument(inspector_port);
        let mut launch_arguments = vec![inspector_arg];
        launch_arguments.extend(runtime_arguments.iter().cloned());
        let mut spawned = spawn_windows_codex(app_dir, debug_port, &launch_arguments).await?;
        match crate::codex_startup_patch::install_with_workflow_proxy(
            inspector_port,
            patch_options,
            runtime_config_overrides,
            workflow_proxy,
        )
        .await
        {
            Ok(()) => {
                spawned.performance_status = "ready".to_string();
                spawned.performance_detail = startup_patch_detail();
                Ok(spawned)
            }
            Err(error) => {
                let patch_error = format!("{error:#}");
                error_log::record_failure(
                    "patch_failed",
                    "install_startup_patch",
                    patch_error.clone(),
                    serde_json::json!({
                        "platform": "windows",
                        "inspectorPort": inspector_port,
                        "processId": spawned.process_id,
                        "disablePet": patch_options.disable_pet,
                        "fastCodexStartup": patch_options.fast_codex_startup,
                    }),
                );
                if let Err(cleanup_error) = stop_windows_spawned_codex(&mut spawned, app_dir).await
                {
                    anyhow::bail!(
                        "Codex 启动补丁未能安装，且无法安全清理暂停的启动进程：{patch_error}；{cleanup_error:#}"
                    );
                }
                if !runtime_config_overrides.is_empty() || subagent_gate_active {
                    anyhow::bail!(
                        "Codex 启动补丁未能安装；为避免丢失 Codey 运行时约束，已停止 Codex：{patch_error}"
                    );
                }
                match spawn_windows_codex(app_dir, debug_port, &runtime_arguments).await {
                    Ok(mut fallback) => {
                        fallback.performance_status = "degraded".to_string();
                        fallback.performance_detail =
                            "启动补丁未能安装，已自动以兼容模式启动；启动优化将在下次启动时重试"
                                .to_string();
                        error_log::record_failure(
                            "patch_degraded",
                            "restart_without_startup_patch",
                            patch_error,
                            serde_json::json!({
                                "platform": "windows",
                                "processId": fallback.process_id,
                            }),
                        );
                        Ok(fallback)
                    }
                    Err(fallback_error) => anyhow::bail!(
                        "Codex 启动补丁未能安装，且兼容模式重启失败：{patch_error}；{fallback_error:#}"
                    ),
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let inspector_port =
            crate::codex_startup_patch::reserve_loopback_port().map_err(|error| {
                let error = error.context("为 macOS Codex 启动补丁选择本地调试端口失败");
                error_log::record_failure(
                    "patch_failed",
                    "reserve_startup_patch_port",
                    format!("{error:#}"),
                    serde_json::json!({
                        "platform": "macos",
                    }),
                );
                error
            })?;
        let inspector_arg = crate::codex_startup_patch::inspector_argument(inspector_port);
        let mut launch_arguments = vec![inspector_arg.clone()];
        launch_arguments.extend(runtime_arguments);
        let command = if app_dir.extension().and_then(|value| value.to_str()) == Some("app") {
            build_fresh_macos_open_command(app_dir, debug_port, &launch_arguments)
        } else {
            build_codex_command(app_dir, debug_port, &launch_arguments)
        };
        let mut spawned = spawn_command(command)?;
        spawned.inspector_argument = Some(inspector_arg.clone());
        match crate::codex_startup_patch::install_with_workflow_proxy(
            inspector_port,
            patch_options,
            runtime_config_overrides,
            workflow_proxy,
        )
        .await
        {
            Ok(()) => {
                spawned.performance_status = "ready".to_string();
                spawned.performance_detail = startup_patch_detail();
                Ok(spawned)
            }
            Err(error) => {
                error_log::record_failure(
                    "patch_failed",
                    "install_startup_patch",
                    format!("{error:#}"),
                    serde_json::json!({
                        "platform": "macos",
                        "inspectorPort": inspector_port,
                        "processId": spawned.process_id,
                        "processGroupId": spawned.process_group_id,
                        "disablePet": patch_options.disable_pet,
                        "fastCodexStartup": patch_options.fast_codex_startup,
                    }),
                );
                if let Err(stop_error) = stop_macos_codex(
                    &inspector_arg,
                    app_dir,
                    spawned.process_id,
                    spawned.process_group_id,
                )
                .await
                {
                    error_log::record_failure(
                        "cleanup_failed",
                        "cleanup_macos_after_startup_patch_failure",
                        format!("{stop_error:#}"),
                        serde_json::json!({
                            "appPath": app_dir,
                            "processId": spawned.process_id,
                            "processGroupId": spawned.process_group_id,
                        }),
                    );
                    eprintln!("Codex 启动补丁失败后的进程清理失败：{stop_error:#}");
                }
                if let Some(child) = spawned.child.take() {
                    reap_child_after_cleanup(child, "reap_child_after_startup_patch_failure").await;
                }
                Err(error).context("Codex 启动补丁未能安装；已停止 Codex")
            }
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let command = build_codex_command(app_dir, debug_port, &runtime_arguments);
        let mut spawned = spawn_command(command)?;
        spawned.performance_status = "ready".to_string();
        spawned.performance_detail = "当前平台无需 macOS / Windows 启动补丁".to_string();
        Ok(spawned)
    }
}

pub(super) async fn reap_child_after_cleanup(mut child: Child, operation: &'static str) {
    let process_id = child.id();
    let needs_kill = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(_)) => false,
        Ok(Err(error)) => {
            error_log::record_failure(
                "cleanup_failed",
                operation,
                error.to_string(),
                serde_json::json!({
                    "processId": process_id,
                    "phase": "wait",
                }),
            );
            true
        }
        Err(_) => true,
    };
    if !needs_kill {
        return;
    }
    if let Err(error) = child.kill().await {
        error_log::record_failure(
            "cleanup_failed",
            operation,
            error.to_string(),
            serde_json::json!({
                "processId": process_id,
                "phase": "kill",
            }),
        );
    }
    if let Err(error) = child.wait().await {
        error_log::record_failure(
            "cleanup_failed",
            operation,
            error.to_string(),
            serde_json::json!({
                "processId": process_id,
                "phase": "wait_after_kill",
            }),
        );
    }
}

pub(super) fn gpu_launch_arguments(
    gpu_launch_mode: GpuLaunchMode,
    enabled_for_platform: bool,
) -> Vec<String> {
    if !enabled_for_platform {
        return Vec::new();
    }

    match gpu_launch_mode {
        GpuLaunchMode::Off => Vec::new(),
        GpuLaunchMode::DisableGpu => vec![DISABLE_GPU_ARGUMENT.to_string()],
        GpuLaunchMode::DisableGpuRasterization => {
            vec![DISABLE_GPU_RASTERIZATION_ARGUMENT.to_string()]
        }
    }
}

pub(super) fn codex_runtime_arguments(
    gpu_launch_mode: GpuLaunchMode,
    gpu_arguments_enabled_for_platform: bool,
) -> Vec<String> {
    let mut arguments = vec![DEFAULT_CHINESE_LOCALE_ARGUMENT.to_string()];
    arguments.extend(gpu_launch_arguments(
        gpu_launch_mode,
        gpu_arguments_enabled_for_platform,
    ));
    arguments
}

pub(super) async fn prepare_codex_for_launch(app_dir: &std::path::Path) -> Result<()> {
    // Startup patches must be applied before the Codex main process starts.
    // If the configured app is already running, stop its process tree and
    // relaunch it under Codey instead of leaving the user to quit it manually.
    #[cfg(windows)]
    {
        let app_dir = app_dir.to_path_buf();
        let process_scan_app_dir = app_dir.clone();
        let already_running = tokio::task::spawn_blocking(move || {
            let executable =
                codey_runtime_core::app_paths::build_codex_executable(&process_scan_app_dir);
            let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
            let executable = normalized_windows_path(&executable);
            codey_runtime_core::windows_enumerate_processes()
                .into_iter()
                .filter_map(|process| process.executable_path)
                .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
                .any(|path| normalized_windows_path(&path) == executable)
        })
        .await
        .context("检测正在运行的 Codex 任务异常退出")?;
        if already_running {
            terminate_windows_codex_processes(&app_dir, None)
                .await
                .context("停止正在运行的 Codex 失败")?;
        }
    }
    #[cfg(not(windows))]
    let _ = app_dir;
    #[cfg(target_os = "macos")]
    if macos_codex_is_running(app_dir).await? {
        terminate_unix_codex_processes(app_dir, None, None, None)
            .await
            .context("停止正在运行的 Codex 失败")?;
    }
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn startup_patch_detail() -> String {
    #[cfg(windows)]
    {
        "Windows 启动补丁已安装：WMI 周期采样保护等待运行时确认，临时 WebView 与执行环境回收已启用"
            .to_string()
    }
    #[cfg(not(windows))]
    {
        "启动补丁已启用：临时 WebView 和执行环境会自动回收".to_string()
    }
}

#[cfg(not(windows))]
fn spawn_command(command: Vec<String>) -> Result<SpawnedCodex> {
    let executable = command
        .first()
        .ok_or_else(|| anyhow::anyhow!("Codex 启动命令为空"))?;
    let mut child_command = Command::new(executable);
    child_command.args(&command[1..]);
    #[cfg(unix)]
    child_command.process_group(0);
    let child = child_command
        .spawn()
        .with_context(|| format!("启动 Codex 失败：{executable}"))?;
    let process_id = child.id();
    Ok(SpawnedCodex {
        child: Some(child),
        process_id,
        #[cfg(unix)]
        process_group_id: process_id,
        #[cfg(target_os = "macos")]
        inspector_argument: None,
        performance_status: String::new(),
        performance_detail: String::new(),
    })
}
