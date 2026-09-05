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
        let mut attempt = 0;
        let mut startup_deadline = None;
        loop {
            attempt += 1;

            let (wrapper, wrapper_preparation_error) =
                match prepare_cli_wrapper(app_dir, subagent_gate_active, runtime_config_overrides)
                    .await
                {
                    Ok(wrapper) => (Some(wrapper), None),
                    Err(error) => {
                        error_log::record_failure(
                            "compatibility_fallback",
                            "prepare_windows_codex_cli_wrapper",
                            format!("{error:#}"),
                            serde_json::json!({ "platform": "windows" }),
                        );
                        (None, Some(error))
                    }
                };
            let deadline = *startup_deadline.get_or_insert_with(|| {
                tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_COMPATIBILITY_TIMEOUT
            });
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "Codex 兼容启动总时限已用尽"
            );
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
            let wrapper_environment = wrapper
                .as_ref()
                .map(|wrapper| wrapper.environment.as_slice())
                .unwrap_or_default();
            let (mut spawned, package_debug_session, wrapper_environment_applied) =
                spawn_windows_codex(app_dir, debug_port, &launch_arguments, wrapper_environment)
                    .await?;
            let wrapper_handshake = wrapper_environment_applied
                .then(|| wrapper.expect("applied wrapper environment should have a listener"))
                .map(CliWrapperLaunch::into_handshake);
            let startup_result = install_startup_patch_with_cli_fallback(
                inspector_port,
                patch_options,
                runtime_config_overrides,
                wrapper_handshake,
                "windows",
                deadline,
            )
            .await
            .map_err(|patch_error| {
                let wrapper_error = wrapper_preparation_error.or_else(|| {
                    (!wrapper_environment_applied)
                        .then(|| anyhow::anyhow!("Windows 未能应用 CLI 兼容环境，详见启动错误日志"))
                });
                match wrapper_error {
                    Some(wrapper_error) => combined_startup_error(patch_error, wrapper_error),
                    None => patch_error,
                }
            });
            let package_cleanup = package_debug_session
                .map(WindowsPackageDebugSession::finish)
                .transpose()
                .map(|_| ());
            let package_cleanup_succeeded = package_cleanup.is_ok();
            let startup_result = match (startup_result, package_cleanup) {
                (mode, Ok(())) => mode,
                (Ok(_), Err(cleanup_error)) => {
                    Err(cleanup_error.context("Windows Store Codex 兼容环境清理失败"))
                }
                (Err(startup_error), Err(cleanup_error)) => Err(anyhow::anyhow!(
                    "{startup_error:#}；Windows Store Codex 兼容环境清理失败：{cleanup_error:#}"
                )),
            };

            match startup_result {
                Ok(()) => {
                    spawned.performance_status = "ready".to_string();
                    spawned.performance_detail = "Codex 启动成功".to_string();
                    return Ok(spawned);
                }
                Err(error) => {
                    let retryable = startup_error_allows_retry(&error);
                    let startup_error = format!("启动尝试 {attempt}/2：{error:#}");
                    error_log::record_failure(
                        "patch_failed",
                        "install_startup_patch_or_cli_wrapper",
                        startup_error.clone(),
                        serde_json::json!({
                            "platform": "windows",
                            "inspectorPort": inspector_port,
                            "processId": spawned.process_id,
                            "startupAttempt": attempt,
                            "retryable": retryable,
                            "remainingBudgetMs": deadline.saturating_duration_since(tokio::time::Instant::now()).as_millis(),
                            "disablePet": patch_options.disable_pet,
                            "runtimeConfigOverrideCount": runtime_config_overrides.len(),
                        }),
                    );
                    if let Err(cleanup_error) =
                        stop_windows_spawned_codex(&mut spawned, app_dir).await
                    {
                        anyhow::bail!(
                            "Codex 启动兼容方案未能安装，且无法安全清理启动进程：{startup_error}；{cleanup_error:#}"
                        );
                    }
                    if !package_cleanup_succeeded {
                        anyhow::bail!(
                            "Codex 启动兼容环境未能安全清理，已停止重试：{startup_error}"
                        );
                    }
                    // 每次重试都重新准备兼容握手和调试端口，清理完成后才能启动。
                    if should_retry_startup(&error, attempt, deadline) {
                        continue;
                    }
                    if !runtime_config_overrides.is_empty() {
                        anyhow::bail!(
                            "Codex 启动兼容方案未能确认 app-server 运行时覆盖；为避免丢失 Codey 运行时约束，已停止 Codex：{startup_error}"
                        );
                    }
                    if subagent_gate_active {
                        anyhow::bail!(
                            "Codex 启动兼容方案未能安装；为避免丢失 Codey 运行时约束，已停止 Codex：{startup_error}"
                        );
                    }
                    match spawn_windows_codex(app_dir, debug_port, &runtime_arguments, &[]).await {
                        Ok((mut fallback, _, _)) => {
                            fallback.performance_status = "degraded".to_string();
                            fallback.performance_detail =
                            "Codex 已启动，但部分启动设置未能应用；页面功能以检测结果为准，下次启动将重试"
                                .to_string();
                            error_log::record_failure(
                                "patch_degraded",
                                "restart_without_startup_patch",
                                startup_error,
                                serde_json::json!({
                                    "platform": "windows",
                                    "processId": fallback.process_id,
                                }),
                            );
                            return Ok(fallback);
                        }
                        Err(fallback_error) => anyhow::bail!(
                            "Codex 启动设置未能应用，且重试启动失败：{startup_error}；{fallback_error:#}"
                        ),
                    }
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
        let wrapper = if app_dir.extension().and_then(|value| value.to_str()) == Some("app") {
            let wrapper =
                prepare_cli_wrapper(app_dir, subagent_gate_active, runtime_config_overrides)
                    .await?;
            add_macos_cli_wrapper(&mut command, &wrapper.environment)?;
            Some(wrapper)
        } else {
            None
        };
        let mut spawned = spawn_command(command)?;
        spawned.inspector_argument = Some(inspector_arg.clone());
        let startup_result = install_startup_patch_with_cli_fallback(
            inspector_port,
            patch_options,
            runtime_config_overrides,
            wrapper.map(CliWrapperLaunch::into_handshake),
            "macos",
            tokio::time::Instant::now() + crate::codex_startup_patch::STARTUP_COMPATIBILITY_TIMEOUT,
        )
        .await;

        match startup_result {
            Ok(()) => {
                spawned.performance_status = "ready".to_string();
                spawned.performance_detail = "Codex 启动成功".to_string();
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
        spawned.performance_detail = "Codex 启动成功".to_string();
        Ok(spawned)
    }
}

#[cfg(any(windows, target_os = "macos"))]
struct CliWrapperLaunch {
    listener: tokio::net::TcpListener,
    token: Vec<u8>,
    environment: Vec<(String, String)>,
}

#[cfg(any(windows, target_os = "macos"))]
impl CliWrapperLaunch {
    fn into_handshake(self) -> (tokio::net::TcpListener, Vec<u8>) {
        (self.listener, self.token)
    }
}

#[cfg(any(windows, test))]
const WINDOWS_CLI_RUNTIME_FILES: [&str; 4] = [
    "codex.exe",
    "codex-code-mode-host.exe",
    "codex-windows-sandbox-setup.exe",
    "codex-command-runner.exe",
];

#[cfg(any(windows, test))]
fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("读取 Codex 运行文件失败：{}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("校验 Codex 运行文件失败：{}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(any(windows, test))]
fn windows_cli_runtime_matches(
    directory: &std::path::Path,
    files: &[(&str, PathBuf, u64, String)],
) -> Result<bool> {
    for (name, _, expected_len, expected_digest) in files {
        let path = directory.join(name);
        let Ok(metadata) = std::fs::metadata(&path) else {
            return Ok(false);
        };
        if !metadata.is_file()
            || metadata.len() != *expected_len
            || sha256_file(&path)? != expected_digest.as_str()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(any(windows, test))]
fn copy_windows_cli_runtime_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<()> {
    let copy_error = match std::fs::copy(source, destination) {
        Ok(_) => return Ok(()),
        Err(error) => error,
    };

    let _ = std::fs::remove_file(destination);
    let buffered_copy = (|| -> std::io::Result<()> {
        let mut input = std::fs::File::open(source)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()
    })();
    buffered_copy.with_context(|| {
        format!(
            "复制受保护的 Codex 运行文件失败：{} -> {}（系统复制错误：{copy_error}）",
            source.display(),
            destination.display()
        )
    })
}

#[cfg(any(windows, test))]
fn stage_windows_cli_runtime(
    target: &std::path::Path,
    local_app_data: &std::path::Path,
) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};

    let source_dir = target.parent().context("Codex CLI 路径缺少父目录")?;
    let mut files = Vec::with_capacity(WINDOWS_CLI_RUNTIME_FILES.len());
    for name in WINDOWS_CLI_RUNTIME_FILES {
        let source = if name == "codex.exe" {
            target.to_path_buf()
        } else {
            source_dir.join(name)
        };
        let metadata = std::fs::metadata(&source)
            .with_context(|| format!("Codex 运行文件缺失：{}", source.display()))?;
        anyhow::ensure!(
            metadata.is_file(),
            "Codex 运行路径不是文件：{}",
            source.display()
        );
        let digest = sha256_file(&source)?;
        files.push((name, source, metadata.len(), digest));
    }

    let mut cache_hasher = Sha256::new();
    for (name, _, _, digest) in &files {
        cache_hasher.update(name.as_bytes());
        cache_hasher.update([0]);
        cache_hasher.update(digest.as_bytes());
        cache_hasher.update([0]);
    }
    let cache_hash = format!("{:x}", cache_hasher.finalize());
    let cache_root = local_app_data.join("OpenAI").join("Codex").join("bin");
    let destination = cache_root.join(&cache_hash[..16]);
    if windows_cli_runtime_matches(&destination, &files)? {
        return Ok(destination.join("codex.exe"));
    }

    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("创建 Codex 用户运行目录失败：{}", cache_root.display()))?;
    if destination.is_dir() {
        std::fs::remove_dir_all(&destination).with_context(|| {
            format!(
                "清理不完整的 Codex 用户运行目录失败：{}",
                destination.display()
            )
        })?;
    } else if destination.exists() {
        std::fs::remove_file(&destination).with_context(|| {
            format!(
                "清理无效的 Codex 用户运行路径失败：{}",
                destination.display()
            )
        })?;
    }

    let staging = cache_root.join(format!(
        ".staging-{}-{}",
        &cache_hash[..16],
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&staging)
        .with_context(|| format!("创建 Codex 运行暂存目录失败：{}", staging.display()))?;
    let result = (|| -> Result<PathBuf> {
        for (name, source, _, expected_digest) in &files {
            let staged = staging.join(name);
            copy_windows_cli_runtime_file(source, &staged)?;
            anyhow::ensure!(
                sha256_file(&staged)? == expected_digest.as_str(),
                "Codex 运行文件复制校验失败：{}",
                staged.display()
            );
        }
        if let Err(error) = std::fs::rename(&staging, &destination) {
            if windows_cli_runtime_matches(&destination, &files)? {
                return Ok(destination.join("codex.exe"));
            }
            return Err(error).with_context(|| {
                format!("启用 Codex 用户运行目录失败：{}", destination.display())
            });
        }
        Ok(destination.join("codex.exe"))
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(any(windows, test))]
fn windows_cli_wrapper_target(app_dir: &std::path::Path) -> Result<PathBuf> {
    let target = codey_runtime_core::app_paths::codex_runtime_executable(app_dir)
        .ok_or_else(|| anyhow::anyhow!("Codex App 内未找到内置 CLI"))?;
    if codey_runtime_core::app_paths::packaged_app_user_model_id(app_dir).is_none() {
        return Ok(target);
    }
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .context("Windows 未提供 LOCALAPPDATA，无法准备 Codex 用户运行目录")?;
    stage_windows_cli_runtime(&target, &local_app_data)
}

#[cfg(any(windows, target_os = "macos"))]
async fn prepare_cli_wrapper(
    app_dir: &std::path::Path,
    subagent_gate_active: bool,
    runtime_config_overrides: &[String],
) -> Result<CliWrapperLaunch> {
    let codey = std::env::current_exe().context("定位 Codey 兼容执行器失败")?;
    #[cfg(windows)]
    let target = {
        let app_dir = app_dir.to_path_buf();
        tokio::task::spawn_blocking(move || windows_cli_wrapper_target(&app_dir))
            .await
            .context("准备 Windows Codex 用户运行文件的任务异常退出")??
    };
    #[cfg(target_os = "macos")]
    let target = codey_runtime_core::app_paths::codex_runtime_executable(app_dir)
        .ok_or_else(|| anyhow::anyhow!("Codex App 内未找到内置 CLI"))?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("创建 Codex CLI 兼容校验端口失败")?;
    let port = listener.local_addr()?.port();
    let token = uuid::Uuid::new_v4().to_string();
    let overrides = serde_json::to_string(runtime_config_overrides)
        .context("序列化 Codex CLI 兼容运行时配置失败")?;
    let mut environment = vec![
        (
            crate::codex_startup_patch::CLI_WRAPPER_TARGET_ENV.to_string(),
            target.to_string_lossy().to_string(),
        ),
        (
            crate::codex_startup_patch::CLI_WRAPPER_OVERRIDES_ENV.to_string(),
            overrides,
        ),
        (
            crate::codex_startup_patch::CLI_WRAPPER_SUBAGENT_ENV.to_string(),
            u8::from(subagent_gate_active).to_string(),
        ),
        (
            crate::codex_startup_patch::CLI_WRAPPER_PORT_ENV.to_string(),
            port.to_string(),
        ),
        (
            crate::codex_startup_patch::CLI_WRAPPER_TOKEN_ENV.to_string(),
            token.clone(),
        ),
    ];
    #[cfg(windows)]
    let wrapper = codey;
    #[cfg(target_os = "macos")]
    let wrapper = {
        let path = crate::config::default_config_path().with_file_name("codex-cli-wrapper");
        write_macos_cli_wrapper(&path, &codey, &environment)?;
        path
    };
    environment.insert(
        0,
        (
            "CODEX_CLI_PATH".to_string(),
            wrapper.to_string_lossy().to_string(),
        ),
    );
    Ok(CliWrapperLaunch {
        listener,
        token: token.into_bytes(),
        environment,
    })
}

#[cfg(target_os = "macos")]
fn write_macos_cli_wrapper(
    path: &std::path::Path,
    codey: &std::path::Path,
    environment: &[(String, String)],
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }

    let mut script = String::from("#!/bin/sh\n");
    for (name, value) in environment {
        script.push_str(&format!("export {name}={}\n", quote(value)));
    }
    script.push_str(&format!(
        "exec {} \"$@\"\n",
        quote(&codey.to_string_lossy())
    ));
    crate::fs_util::atomic_write_private_with_parent(path, script.as_bytes())
        .with_context(|| format!("写入 macOS Codex CLI 兼容入口失败：{}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("设置 macOS Codex CLI 兼容入口权限失败：{}", path.display()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn add_macos_cli_wrapper(
    command: &mut Vec<String>,
    environment: &[(String, String)],
) -> Result<()> {
    let args_index = command
        .iter()
        .position(|argument| argument == "--args")
        .ok_or_else(|| anyhow::anyhow!("macOS Codex 启动命令缺少 --args"))?;
    command.splice(
        args_index..args_index,
        environment
            .iter()
            .flat_map(|(name, value)| ["--env".to_string(), format!("{name}={value}")]),
    );
    Ok(())
}

#[cfg(any(windows, target_os = "macos", test))]
fn startup_error_allows_retry(error: &anyhow::Error) -> bool {
    if let Some(failure) = error.downcast_ref::<crate::codex_startup_patch::CliWrapperFailure>() {
        return failure.retryable;
    }
    error.is::<tokio::time::error::Elapsed>()
        || error
            .downcast_ref::<std::io::Error>()
            .is_some_and(crate::codex_startup_patch::is_retryable_startup_io_error)
}

#[cfg(any(windows, test))]
fn should_retry_startup(
    error: &anyhow::Error,
    attempt: u32,
    deadline: tokio::time::Instant,
) -> bool {
    attempt < 2 && tokio::time::Instant::now() < deadline && startup_error_allows_retry(error)
}

#[cfg(any(windows, target_os = "macos", test))]
fn combined_startup_error(
    patch_error: anyhow::Error,
    wrapper_error: anyhow::Error,
) -> anyhow::Error {
    let kind =
        if startup_error_allows_retry(&patch_error) && startup_error_allows_retry(&wrapper_error) {
            std::io::ErrorKind::TimedOut
        } else {
            std::io::ErrorKind::Other
        };
    std::io::Error::new(
        kind,
        format!("Codex 启动补丁失败：{patch_error:#}；CLI 兼容入口失败：{wrapper_error:#}"),
    )
    .into()
}

#[cfg(any(windows, target_os = "macos"))]
async fn install_startup_patch_with_cli_fallback(
    inspector_port: u16,
    patch_options: crate::codex_startup_patch::PatchOptions,
    runtime_config_overrides: &[String],
    wrapper_handshake: Option<(tokio::net::TcpListener, Vec<u8>)>,
    platform: &'static str,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let mut patch_install = Box::pin(async {
        tokio::time::timeout_at(
            deadline,
            crate::codex_startup_patch::install(
                inspector_port,
                patch_options,
                runtime_config_overrides,
                !runtime_config_overrides.is_empty(),
            ),
        )
        .await
        .context("Codex 兼容启动总时限已用尽")?
    });
    let Some((listener, token)) = wrapper_handshake else {
        return patch_install.as_mut().await;
    };
    let mut wrapper_ready = Box::pin(wait_for_cli_wrapper(listener, token, deadline));
    tokio::select! {
        patch = &mut patch_install => match patch {
            Ok(()) => Ok(()),
            Err(patch_error) => match wrapper_ready.as_mut().await {
                Ok(()) => {
                    error_log::record_failure(
                        "patch_degraded",
                        "use_codex_cli_wrapper_after_patch_failure",
                        format!("{patch_error:#}"),
                        serde_json::json!({ "platform": platform }),
                    );
                    Ok(())
                }
                Err(wrapper_error) if wrapper_error.is::<crate::codex_startup_patch::CliWrapperFailure>() => Err(wrapper_error),
                Err(wrapper_error) => Err(combined_startup_error(patch_error, wrapper_error)),
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
                        Ok(()) => Ok(()),
                        Err(patch_error) => {
                            error_log::record_failure(
                                "patch_degraded",
                                "use_codex_cli_wrapper_after_patch_failure",
                                format!("{patch_error:#}"),
                                serde_json::json!({ "platform": platform }),
                            );
                            Ok(())
                        }
                    }
                } else {
                    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                        "launcher.startup_compatibility_mode",
                        serde_json::json!({
                            "platform": platform,
                            "reason": "main_process_inspector_unavailable",
                            "inspectorPort": inspector_port,
                            "runtimeConfigOverrideCount": runtime_config_overrides.len(),
                        }),
                    );
                    Ok(())
                }
            },
            Err(wrapper_error) if wrapper_error.is::<crate::codex_startup_patch::CliWrapperFailure>() => Err(wrapper_error),
            Err(wrapper_error) => match patch_install.as_mut().await {
                Ok(()) => Ok(()),
                Err(patch_error) => Err(combined_startup_error(patch_error, wrapper_error)),
            },
        },
    }
}

#[cfg(any(windows, target_os = "macos"))]
async fn wait_for_cli_wrapper(
    listener: tokio::net::TcpListener,
    expected_token: Vec<u8>,
    deadline: tokio::time::Instant,
) -> Result<()> {
    use crate::codex_startup_patch::{
        CliWrapperFailure, MAX_CLI_WRAPPER_FAILURE_BYTES, STARTUP_READY_TIMEOUT,
    };
    use tokio::io::AsyncReadExt;

    let deadline = deadline.min(tokio::time::Instant::now() + STARTUP_READY_TIMEOUT);
    tokio::time::timeout_at(deadline, async {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut received = vec![0; expected_token.len()];
            if tokio::time::timeout(Duration::from_millis(750), stream.read_exact(&mut received))
                .await
                .is_ok_and(|result| result.is_ok())
                && received == expected_token
            {
                let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                    "launcher.cli_wrapper_authenticated",
                    serde_json::json!({ "remainingBudgetMs": deadline.saturating_duration_since(tokio::time::Instant::now()).as_millis() }),
                );
                // 令牌只证明包装器已进入启动流程，创建目标进程仍共享外层截止时间。
                let mut status = [0];
                let end = stream
                    .read(&mut status)
                    .await
                    .context("读取 Codex CLI 执行确认失败")?;
                if end == 0 {
                    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                        "launcher.cli_wrapper_exec_confirmed", serde_json::json!({}),
                    );
                    return Ok::<_, anyhow::Error>(());
                }
                let mut body = Vec::new();
                let payload = tokio::time::timeout(
                    Duration::from_millis(750),
                    stream
                        .take((MAX_CLI_WRAPPER_FAILURE_BYTES + 1) as u64)
                        .read_to_end(&mut body),
                )
                .await;
                let failure = if status[0] == b'!'
                    && payload.is_ok_and(|result| result.is_ok())
                    && body.len() <= MAX_CLI_WRAPPER_FAILURE_BYTES
                {
                    serde_json::from_slice::<CliWrapperFailure>(&body).ok()
                } else {
                    None
                }
                .unwrap_or_else(|| CliWrapperFailure {
                    message: "目标程序未能执行，未收到完整的失败详情".to_string(),
                    retryable: false,
                });
                return Err(failure.into());
            }
        }
    })
    .await
    .context("等待 Codex CLI 兼容执行器超时")?
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

#[cfg(test)]
mod cli_wrapper_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn startup_retry_requires_a_transient_error_and_remaining_budget() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let timeout = || {
            anyhow::Error::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "not ready",
            ))
        };
        let transient = crate::codex_startup_patch::CliWrapperFailure {
            message: "运行文件暂时被占用".to_string(),
            retryable: true,
        }
        .into();
        let invalid = crate::codex_startup_patch::CliWrapperFailure {
            message: "运行时配置无效".to_string(),
            retryable: false,
        }
        .into();
        for (code, retryable) in [
            (5, false),
            (193, false),
            (32, cfg!(windows)),
            (33, cfg!(windows)),
        ] {
            let error = std::io::Error::from_raw_os_error(code).into();
            assert_eq!(startup_error_allows_retry(&error), retryable);
        }
        assert!(should_retry_startup(&timeout(), 1, deadline));
        assert!(should_retry_startup(&transient, 1, deadline));
        assert!(!should_retry_startup(&invalid, 1, deadline));
        assert!(!should_retry_startup(&timeout(), 2, deadline));
        assert!(!startup_error_allows_retry(&combined_startup_error(
            anyhow::anyhow!("invalid inspector response"),
            timeout()
        )));
        assert!(startup_error_allows_retry(&combined_startup_error(
            timeout(),
            timeout()
        )));
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(!should_retry_startup(&timeout(), 1, deadline));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn compatibility_waits_share_the_callers_deadline() {
        let port = crate::codex_startup_patch::reserve_loopback_port().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(80);
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            install_startup_patch_with_cli_fallback(
                port,
                crate::codex_startup_patch::PatchOptions {
                    disable_pet: false,
                    subagent_gate_active: true,
                },
                &["analytics.enabled=false".to_string()],
                Some((listener, b"token".to_vec())),
                "windows",
                deadline,
            ),
        )
        .await
        .expect("neither compatibility path may reset the caller's deadline");
        assert!(startup_error_allows_retry(&result.unwrap_err()));
        assert!(tokio::time::Instant::now() >= deadline);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn cli_launch_failure_returns_its_cause_without_waiting_for_inspector() {
        use tokio::io::AsyncWriteExt;
        for retryable in [false, true] {
            let port = crate::codex_startup_patch::reserve_loopback_port().unwrap();
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let sender = tokio::spawn(async move {
                let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
                stream.write_all(b"token!").await.unwrap();
                stream
                    .write_all(
                        serde_json::to_string(&crate::codex_startup_patch::CliWrapperFailure {
                            message: "CreateProcess failed: os error 193".to_string(),
                            retryable,
                        })
                        .unwrap()
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                install_startup_patch_with_cli_fallback(
                    port,
                    crate::codex_startup_patch::PatchOptions {
                        disable_pet: false,
                        subagent_gate_active: true,
                    },
                    &[],
                    Some((listener, b"token".to_vec())),
                    "windows",
                    tokio::time::Instant::now()
                        + crate::codex_startup_patch::STARTUP_COMPATIBILITY_TIMEOUT,
                ),
            )
            .await
            .expect("an explicit CLI launch failure must return immediately");
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains("os error 193"));
            assert_eq!(startup_error_allows_retry(&error), retryable);
            sender.await.unwrap();
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn cli_handshake_requires_authenticated_exec_completion() {
        use tokio::io::AsyncWriteExt;

        for failed in [false, true] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let sender = tokio::spawn(async move {
                let mut invalid = tokio::net::TcpStream::connect(address).await.unwrap();
                invalid.write_all(b"invalid").await.unwrap();
                drop(invalid);
                let mut valid = tokio::net::TcpStream::connect(address).await.unwrap();
                valid.write_all(b"token").await.unwrap();
                if failed {
                    valid.write_all(b"!").await.unwrap();
                } else {
                    // 创建进程超过旧的 750ms 窗口，仍应等到明确的执行结果。
                    tokio::time::sleep(Duration::from_millis(900)).await;
                }
            });
            let result = wait_for_cli_wrapper(
                listener,
                b"token".to_vec(),
                tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_COMPATIBILITY_TIMEOUT,
            )
            .await;
            assert_eq!(result.is_err(), failed);
            sender.await.unwrap();
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test(start_paused = true)]
    async fn cli_fallback_accepts_cold_start_within_readiness_deadline() {
        use tokio::io::AsyncWriteExt;

        // 保持 Inspector 不可用，让测试覆盖实际的 CLI 兼容启动路径。
        let inspector = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let inspector_port = inspector.local_addr().unwrap().port();
        drop(inspector);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(18)).await;
            tokio::time::resume();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(b"token").await.unwrap();
        });
        install_startup_patch_with_cli_fallback(
            inspector_port,
            crate::codex_startup_patch::PatchOptions {
                disable_pet: false,
                subagent_gate_active: true,
            },
            &["analytics.enabled=false".to_string()],
            Some((listener, b"token".to_vec())),
            "windows",
            tokio::time::Instant::now() + crate::codex_startup_patch::STARTUP_COMPATIBILITY_TIMEOUT,
        )
        .await
        .unwrap();
        sender.await.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cli_wrapper_restores_environment_after_codex_filters_it() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let codey = temp.path().join("fake codey's executable");
        std::fs::write(
            &codey,
            "#!/bin/sh\nprintf '%s\\n' \"$CODEY_CODEX_CLI_WRAPPER_TARGET\" \"$1\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&codey, std::fs::Permissions::from_mode(0o700)).unwrap();
        let expected = "target with ' quote";
        let wrapper = temp.path().join("codex-cli-wrapper");
        write_macos_cli_wrapper(
            &wrapper,
            &codey,
            &[(
                crate::codex_startup_patch::CLI_WRAPPER_TARGET_ENV.to_string(),
                expected.to_string(),
            )],
        )
        .unwrap();

        let output = std::process::Command::new(&wrapper)
            .env_clear()
            .arg("app-server")
            .output()
            .unwrap();

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{expected}\napp-server\n")
        );
        assert_eq!(
            std::fs::metadata(wrapper).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn windows_cli_runtime_is_staged_with_all_required_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let resources = temp.path().join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        for name in WINDOWS_CLI_RUNTIME_FILES {
            std::fs::write(resources.join(name), format!("payload:{name}")).unwrap();
        }

        let target = resources.join("codex.exe");
        assert_eq!(windows_cli_wrapper_target(temp.path()).unwrap(), target);
        let local_app_data = temp.path().join("local-app-data");
        let staged = stage_windows_cli_runtime(&target, &local_app_data).unwrap();
        assert!(staged.starts_with(local_app_data.join("OpenAI/Codex/bin")));
        assert_eq!(
            staged.parent().unwrap().file_name().unwrap(),
            "657d1ed8f1a42bf7"
        );
        for name in WINDOWS_CLI_RUNTIME_FILES {
            assert_eq!(
                std::fs::read(staged.parent().unwrap().join(name)).unwrap(),
                format!("payload:{name}").as_bytes()
            );
        }
        assert_eq!(
            stage_windows_cli_runtime(&target, &local_app_data).unwrap(),
            staged
        );
    }
}
