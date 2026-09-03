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

pub(super) async fn spawn_codex(
    app_dir: &std::path::Path,
    debug_port: u16,
    disable_codex_pet: bool,
    subagent_gate_active: bool,
    gpu_launch_mode: GpuLaunchMode,
    runtime_config_overrides: &[String],
) -> Result<SpawnedCodex> {
    #[cfg(any(windows, target_os = "macos"))]
    let patch_options = crate::codex_startup_patch::PatchOptions {
        disable_pet: disable_codex_pet,
        subagent_gate_active,
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = (
        disable_codex_pet,
        subagent_gate_active,
        runtime_config_overrides,
    );
    let runtime_arguments =
        codex_runtime_arguments(gpu_launch_mode, !cfg!(target_os = "macos"), cfg!(windows));

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
        match crate::codex_startup_patch::install(
            inspector_port,
            patch_options,
            runtime_config_overrides,
            !runtime_config_overrides.is_empty(),
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
                        "runtimeConfigOverrideCount": runtime_config_overrides.len(),
                    }),
                );
                if let Err(cleanup_error) = stop_windows_spawned_codex(&mut spawned, app_dir).await
                {
                    anyhow::bail!(
                        "Codex 启动补丁未能安装，且无法安全清理暂停的启动进程：{patch_error}；{cleanup_error:#}"
                    );
                }
                if !runtime_config_overrides.is_empty() {
                    anyhow::bail!(
                        "Codex 启动补丁未能确认 app-server 运行时覆盖；为避免丢失 Codey 运行时约束，已停止 Codex。当前 Codex 版本可能与 Codey 不兼容：{patch_error}"
                    );
                }
                if subagent_gate_active {
                    anyhow::bail!(
                        "Codex 启动补丁未能安装；为避免丢失 Codey 运行时约束，已停止 Codex：{patch_error}"
                    );
                }
                match spawn_windows_codex(app_dir, debug_port, &runtime_arguments).await {
                    Ok(mut fallback) => {
                        fallback.performance_status = "degraded".to_string();
                        fallback.performance_detail =
                            "启动补丁未能安装，已自动以兼容模式启动；本次会话的 Windows Git、WMI 与隐藏宠物窗口优化未生效，下次启动将重试"
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
        launch_arguments.extend(runtime_arguments.iter().cloned());
        let mut command = if app_dir.extension().and_then(|value| value.to_str()) == Some("app") {
            build_fresh_macos_open_command(app_dir, debug_port, &launch_arguments)
        } else {
            build_codex_command(app_dir, debug_port, &launch_arguments)
        };
        let wrapper_handshake =
            if app_dir.extension().and_then(|value| value.to_str()) == Some("app") {
                Some(
                    add_macos_cli_wrapper(
                        &mut command,
                        app_dir,
                        subagent_gate_active,
                        runtime_config_overrides,
                    )
                    .await?,
                )
            } else {
                None
            };
        let mut spawned = spawn_command(command)?;
        spawned.inspector_argument = Some(inspector_arg.clone());
        let mut patch_install = Box::pin(crate::codex_startup_patch::install(
            inspector_port,
            patch_options,
            runtime_config_overrides,
            !runtime_config_overrides.is_empty(),
        ));
        let startup_mode = if let Some((listener, token)) = wrapper_handshake {
            let mut wrapper_ready = Box::pin(wait_for_macos_cli_wrapper(listener, token));
            tokio::select! {
                patch = &mut patch_install => match patch {
                    Ok(()) => Ok(false),
                    Err(patch_error) => match wrapper_ready.as_mut().await {
                        Ok(()) => {
                            error_log::record_failure(
                                "patch_degraded",
                                "use_codex_cli_wrapper_after_patch_failure",
                                format!("{patch_error:#}"),
                                serde_json::json!({ "platform": "macos" }),
                            );
                            Ok(true)
                        }
                        Err(wrapper_error) => Err(anyhow::anyhow!(
                            "Codex 启动补丁失败：{patch_error:#}；CLI 兼容入口失败：{wrapper_error:#}"
                        )),
                    },
                },
                wrapper = &mut wrapper_ready => match wrapper {
                    Ok(()) => {
                        let inspector_is_active = tokio::time::timeout(
                            Duration::from_millis(200),
                            tokio::net::TcpStream::connect(("127.0.0.1", inspector_port)),
                        )
                        .await
                        .is_ok_and(|result| result.is_ok());
                        if inspector_is_active {
                            match patch_install.as_mut().await {
                                Ok(()) => Ok(false),
                                Err(patch_error) => {
                                    error_log::record_failure(
                                        "patch_degraded",
                                        "use_codex_cli_wrapper_after_patch_failure",
                                        format!("{patch_error:#}"),
                                        serde_json::json!({ "platform": "macos" }),
                                    );
                                    Ok(true)
                                }
                            }
                        } else {
                            Ok(true)
                        }
                    },
                    Err(wrapper_error) => match patch_install.as_mut().await {
                        Ok(()) => Ok(false),
                        Err(patch_error) => Err(anyhow::anyhow!(
                            "Codex CLI 兼容入口失败：{wrapper_error:#}；启动补丁失败：{patch_error:#}"
                        )),
                    },
                },
            }
        } else {
            patch_install.as_mut().await.map(|()| false)
        };

        match startup_mode {
            Ok(false) => {
                spawned.performance_status = "ready".to_string();
                spawned.performance_detail = startup_patch_detail();
                Ok(spawned)
            }
            Ok(true) => {
                spawned.performance_status = "degraded".to_string();
                spawned.performance_detail =
                    "新版兼容模式已启用：运行时线路、FastCtx、子代理与页面注入保持可用；Codex 已禁用主进程调试，因此临时 WebView、执行环境回收和预加载界面补丁本次未启用"
                        .to_string();
                Ok(spawned)
            }
            Err(error) => {
                error_log::record_failure(
                    "patch_failed",
                    "install_startup_patch_or_cli_wrapper",
                    format!("{error:#}"),
                    serde_json::json!({
                        "platform": "macos",
                        "inspectorPort": inspector_port,
                        "processId": spawned.process_id,
                        "processGroupId": spawned.process_group_id,
                        "disablePet": patch_options.disable_pet,
                    }),
                );
                let stop_result = stop_macos_codex(
                    &inspector_arg,
                    app_dir,
                    spawned.process_id,
                    spawned.process_group_id,
                )
                .await;
                if let Err(stop_error) = &stop_result {
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
                if let Err(stop_error) = stop_result {
                    anyhow::bail!(
                        "Codex 启动兼容方案未能安装，且无法安全清理旧进程：{error:#}；{stop_error:#}"
                    );
                }
                Err(error).context("Codex 启动兼容方案未能安装；已停止 Codex")
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

#[cfg(target_os = "macos")]
async fn add_macos_cli_wrapper(
    command: &mut Vec<String>,
    app_dir: &std::path::Path,
    subagent_gate_active: bool,
    runtime_config_overrides: &[String],
) -> Result<(tokio::net::TcpListener, Vec<u8>)> {
    let wrapper = std::env::current_exe().context("定位 Codey 兼容执行器失败")?;
    let target = codey_runtime_core::app_paths::codex_runtime_executable(app_dir)
        .ok_or_else(|| anyhow::anyhow!("Codex App 内未找到内置 CLI"))?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("创建 Codex CLI 兼容校验端口失败")?;
    let port = listener.local_addr()?.port();
    let token = uuid::Uuid::new_v4().to_string();
    let overrides = serde_json::to_string(runtime_config_overrides)
        .context("序列化 Codex CLI 兼容运行时配置失败")?;

    let args_index = command
        .iter()
        .position(|argument| argument == "--args")
        .ok_or_else(|| anyhow::anyhow!("macOS Codex 启动命令缺少 --args"))?;
    let environment = [
        format!("CODEX_CLI_PATH={}", wrapper.to_string_lossy()),
        format!(
            "{}={}",
            crate::codex_startup_patch::CLI_WRAPPER_TARGET_ENV,
            target.to_string_lossy()
        ),
        format!(
            "{}={overrides}",
            crate::codex_startup_patch::CLI_WRAPPER_OVERRIDES_ENV
        ),
        format!(
            "{}={}",
            crate::codex_startup_patch::CLI_WRAPPER_SUBAGENT_ENV,
            u8::from(subagent_gate_active)
        ),
        format!(
            "{}={port}",
            crate::codex_startup_patch::CLI_WRAPPER_PORT_ENV
        ),
        format!(
            "{}={token}",
            crate::codex_startup_patch::CLI_WRAPPER_TOKEN_ENV
        ),
    ];
    command.splice(
        args_index..args_index,
        environment
            .into_iter()
            .flat_map(|value| ["--env".to_string(), value]),
    );
    Ok((listener, token.into_bytes()))
}

#[cfg(target_os = "macos")]
async fn wait_for_macos_cli_wrapper(
    listener: tokio::net::TcpListener,
    expected_token: Vec<u8>,
) -> Result<()> {
    use tokio::io::AsyncReadExt;

    match tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut received = vec![0; expected_token.len()];
            if tokio::time::timeout(Duration::from_millis(750), stream.read_exact(&mut received))
                .await
                .is_ok_and(|result| result.is_ok())
                && received == expected_token
            {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("等待 Codex CLI 兼容执行器超时")),
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
    disable_background_ecoqos: bool,
) -> Vec<String> {
    let mut arguments = vec![DEFAULT_CHINESE_LOCALE_ARGUMENT.to_string()];
    if disable_background_ecoqos {
        // Chromium marks backgrounded renderer processes as EcoQoS on Windows
        // 11. During Codex startup that can throttle the renderer which owns the
        // app:// module patch and CDP bridge, so keep the controlled process tree
        // on the normal scheduler policy.
        arguments.push(DISABLE_BACKGROUND_ECOQOS_ARGUMENT.to_string());
    }
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
