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
        let inspect_fuse =
            crate::electron_fuses::detect_node_cli_inspect_state(app_dir.to_path_buf()).await;
        // Electron drops `--inspect-brk` when the fuse is off, so the Inspector
        // patch can never attach on such builds. Start on the CLI wrapper right
        // away instead of waiting for a debug port that will never answer.
        let mut cli_only = !inspect_fuse.inspector_possible();
        let mut attempt = 0;
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
            let inspector_port = if cli_only {
                None
            } else {
                Some(
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
                    })?,
                )
            };
            if inspector_port.is_none() && wrapper.is_none() {
                // Neither compatibility entry exists before launch: decide now
                // instead of starting a process that would only be stopped again.
                let error = wrapper_preparation_error
                    .unwrap_or_else(|| anyhow::anyhow!("Codex CLI 兼容入口不可用"));
                return launch_windows_codex_without_compatibility(
                    app_dir,
                    debug_port,
                    &runtime_arguments,
                    runtime_config_overrides,
                    subagent_gate_active,
                    format!(
                        "启动尝试 {attempt}/2：主进程 Inspector 已被 Electron fuse 关闭（{}），且 CLI 兼容入口不可用：{error:#}",
                        inspect_fuse.as_str()
                    ),
                )
                .await;
            }
            let launch_arguments = startup_launch_arguments(&runtime_arguments, inspector_port);
            let wrapper_environment = wrapper
                .as_ref()
                .map(|wrapper| wrapper.environment.as_slice())
                .unwrap_or_default();
            // Without an Inspector the wrapper is the only entry, so a launch
            // that carries runtime constraints must not proceed unless Store
            // accepts the wrapper environment; an unconstrained launch may.
            let constrained = !runtime_config_overrides.is_empty() || subagent_gate_active;
            let (mut spawned, package_debug_session, wrapper_environment_applied) =
                spawn_windows_codex(
                    app_dir,
                    debug_port,
                    &launch_arguments,
                    wrapper_environment,
                    cli_only && constrained,
                )
                .await?;
            // Each attempt gets its own readiness budget. Cleanup and Store
            // activation must not consume the next attempt's window.
            let deadline =
                tokio::time::Instant::now() + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT;
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_startup_attempt",
                serde_json::json!({
                    "attempt": attempt, "cliOnly": cli_only,
                    "inspectorFuse": inspect_fuse.as_str(),
                    "processId": spawned.process_id,
                    "wrapperEnvironmentApplied": wrapper_environment_applied,
                }),
            );
            let wrapper_handshake = wrapper_environment_applied
                .then(|| wrapper.expect("applied wrapper environment should have a listener"))
                .map(CliWrapperLaunch::into_handshake);
            if inspector_port.is_none() && wrapper_handshake.is_none() {
                // The process is already running without any compatibility
                // entry. It carries no constraints (see above), so keep it
                // instead of stopping and relaunching the same configuration.
                if let Some(session) = package_debug_session {
                    session
                        .finish()
                        .context("Windows Store Codex 兼容环境清理失败")?;
                }
                let startup_error = format!(
                    "启动尝试 {attempt}/2：主进程 Inspector 已被 Electron fuse 关闭（{}），且 Windows 未能应用 CLI 兼容环境，详见启动错误日志",
                    inspect_fuse.as_str()
                );
                spawned.performance_status = "degraded".to_string();
                spawned.performance_detail =
                    "Codex 已启动，但部分启动设置未能应用；页面功能以检测结果为准，下次启动将重试"
                        .to_string();
                error_log::record_failure(
                    "patch_degraded",
                    "start_without_startup_patch",
                    startup_error,
                    serde_json::json!({
                        "platform": "windows",
                        "processId": spawned.process_id,
                    }),
                );
                return Ok(spawned);
            }
            let startup_result = install_startup_patch_with_cli_fallback(
                inspector_port,
                patch_options,
                runtime_config_overrides,
                wrapper_handshake,
                StartupWaitContext {
                    platform: "windows",
                    deadline,
                    renderer_debug_port: Some(debug_port),
                    spawned: Some(&mut spawned),
                },
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
                            "inspectorFuse": inspect_fuse.as_str(),
                            "processId": spawned.process_id,
                            "startupAttempt": attempt,
                            "cliOnly": cli_only,
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
                    // A main process paused at an unreachable `--inspect-brk`,
                    // a lost handshake or an early exit all get one more attempt
                    // without the breakpoint; the wrapper is prepared again.
                    if should_retry_startup(&error, attempt) {
                        cli_only = true;
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
                    match spawn_windows_codex(app_dir, debug_port, &runtime_arguments, &[], false)
                        .await
                    {
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
        let inspect_fuse =
            crate::electron_fuses::detect_node_cli_inspect_state(app_dir.to_path_buf()).await;
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
        // The Inspector argument stays on the command line as the cleanup
        // marker, but Electron drops it when the fuse is off: wait for the CLI
        // wrapper alone in that case instead of a port that never answers.
        let startup_result = install_startup_patch_with_cli_fallback(
            inspect_fuse.inspector_possible().then_some(inspector_port),
            patch_options,
            runtime_config_overrides,
            wrapper.map(CliWrapperLaunch::into_handshake),
            StartupWaitContext {
                platform: "macos",
                deadline: tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                renderer_debug_port: Some(debug_port),
                spawned: Some(&mut spawned),
            },
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
                        "inspectorFuse": inspect_fuse.as_str(),
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
    marker_path: PathBuf,
    environment: Vec<(String, String)>,
}

#[cfg(any(windows, target_os = "macos"))]
impl CliWrapperLaunch {
    fn into_handshake(self) -> CliWrapperHandshake {
        CliWrapperHandshake {
            listener: self.listener,
            token: self.token,
            marker_path: self.marker_path,
        }
    }
}

/// Two independent confirmation channels for the CLI wrapper: the loopback
/// handshake connection and a marker file it writes next to Codey's state.
#[cfg(any(windows, target_os = "macos"))]
struct CliWrapperHandshake {
    listener: tokio::net::TcpListener,
    token: Vec<u8>,
    marker_path: PathBuf,
}

/// Evidence available while waiting for a compatibility entry to confirm.
#[cfg(any(windows, target_os = "macos"))]
struct StartupWaitContext<'a> {
    platform: &'static str,
    deadline: tokio::time::Instant,
    /// Chromium's `--remote-debugging-port`; once it answers, the main script
    /// has started, so a refused Inspector port will never open.
    renderer_debug_port: Option<u16>,
    /// The launched process, polled so a crash or single-instance handoff ends
    /// the wait immediately instead of at the deadline.
    spawned: Option<&'a mut SpawnedCodex>,
}

#[cfg(any(windows, target_os = "macos"))]
const CLI_WRAPPER_MARKER_DIR: &str = "cli-wrapper";
#[cfg(any(windows, target_os = "macos", test))]
const CLI_WRAPPER_MARKER_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Removes marker files left behind by launches that never reached cleanup.
#[cfg(any(windows, target_os = "macos", test))]
fn prune_cli_wrapper_markers(
    directory: &std::path::Path,
    now: std::time::SystemTime,
    max_age: Duration,
) -> usize {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten().take(1024) {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= max_age);
        if stale && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(any(windows, target_os = "macos"))]
async fn prepare_cli_wrapper_marker(token: &str) -> PathBuf {
    let directory = codey_runtime_core::paths::default_app_state_dir().join(CLI_WRAPPER_MARKER_DIR);
    let marker_path = directory.join(format!("{token}.json"));
    let prune_directory = directory.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let _ = std::fs::create_dir_all(&prune_directory);
        prune_cli_wrapper_markers(
            &prune_directory,
            std::time::SystemTime::now(),
            CLI_WRAPPER_MARKER_MAX_AGE,
        )
    })
    .await;
    marker_path
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
const STAGED_RUNTIME_MANIFEST: &str = ".codey-staged.json";
#[cfg(any(windows, test))]
const STAGED_RUNTIME_MANIFEST_VERSION: u32 = 1;

#[cfg(any(windows, test))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct StagedRuntimeFile {
    name: String,
    len: u64,
    source_modified_ms: Option<u64>,
    sha256: String,
}

#[cfg(any(windows, test))]
#[derive(serde::Serialize, serde::Deserialize)]
struct StagedRuntimeManifest {
    version: u32,
    files: Vec<StagedRuntimeFile>,
}

#[cfg(any(windows, test))]
struct WindowsCliRuntimeSource {
    name: &'static str,
    path: PathBuf,
    len: u64,
    modified_ms: Option<u64>,
}

#[cfg(any(windows, test))]
fn file_modified_ms(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(any(windows, test))]
fn windows_cli_runtime_sources(target: &std::path::Path) -> Result<Vec<WindowsCliRuntimeSource>> {
    let source_dir = target.parent().context("Codex CLI 路径缺少父目录")?;
    let mut sources = Vec::with_capacity(WINDOWS_CLI_RUNTIME_FILES.len());
    for name in WINDOWS_CLI_RUNTIME_FILES {
        let path = if name == "codex.exe" {
            target.to_path_buf()
        } else {
            source_dir.join(name)
        };
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("Codex 运行文件缺失：{}", path.display()))?;
        anyhow::ensure!(
            metadata.is_file(),
            "Codex 运行路径不是文件：{}",
            path.display()
        );
        sources.push(WindowsCliRuntimeSource {
            name,
            path,
            len: metadata.len(),
            modified_ms: file_modified_ms(&metadata),
        });
    }
    Ok(sources)
}

/// A staged directory is reusable when its manifest still describes the current
/// package files and every copy has the recorded size. Store packages are
/// immutable per version, so size and modification time identify the sources
/// without re-hashing several hundred megabytes on every launch; content is
/// verified once, when the copy is made.
#[cfg(any(windows, test))]
fn staged_runtime_ready(
    destination: &std::path::Path,
    sources: &[WindowsCliRuntimeSource],
) -> bool {
    let Ok(bytes) = std::fs::read(destination.join(STAGED_RUNTIME_MANIFEST)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<StagedRuntimeManifest>(&bytes) else {
        return false;
    };
    if manifest.version != STAGED_RUNTIME_MANIFEST_VERSION || manifest.files.len() != sources.len()
    {
        return false;
    }
    sources.iter().all(|source| {
        let recorded = manifest.files.iter().any(|file| {
            file.name == source.name
                && file.len == source.len
                && file.source_modified_ms == source.modified_ms
        });
        recorded
            && std::fs::metadata(destination.join(source.name))
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == source.len)
    })
}

#[cfg(any(windows, test))]
fn stage_windows_cli_runtime(
    target: &std::path::Path,
    local_app_data: &std::path::Path,
) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};

    let sources = windows_cli_runtime_sources(target)?;
    let mut cache_hasher = Sha256::new();
    for source in &sources {
        cache_hasher.update(source.name.as_bytes());
        cache_hasher.update([0]);
        cache_hasher.update(source.len.to_le_bytes());
        cache_hasher.update([0]);
        cache_hasher.update(source.modified_ms.unwrap_or(0).to_le_bytes());
        cache_hasher.update([0]);
    }
    let cache_hash = format!("{:x}", cache_hasher.finalize());
    let cache_root = local_app_data.join("OpenAI").join("Codex").join("bin");
    let destination = cache_root.join(&cache_hash[..16]);
    if staged_runtime_ready(&destination, &sources) {
        return Ok(destination.join("codex.exe"));
    }

    // Slow path: a new Codex build or a damaged copy. Hash, copy, verify, then
    // publish the directory atomically together with its manifest.
    let mut files = Vec::with_capacity(sources.len());
    for source in &sources {
        files.push(StagedRuntimeFile {
            name: source.name.to_string(),
            len: source.len,
            source_modified_ms: source.modified_ms,
            sha256: sha256_file(&source.path)?,
        });
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
        for (source, file) in sources.iter().zip(&files) {
            let staged = staging.join(source.name);
            copy_windows_cli_runtime_file(&source.path, &staged)?;
            anyhow::ensure!(
                sha256_file(&staged)? == file.sha256,
                "Codex 运行文件复制校验失败：{}",
                staged.display()
            );
        }
        let manifest = StagedRuntimeManifest {
            version: STAGED_RUNTIME_MANIFEST_VERSION,
            files: files.clone(),
        };
        std::fs::write(
            staging.join(STAGED_RUNTIME_MANIFEST),
            serde_json::to_vec(&manifest)?,
        )
        .with_context(|| format!("写入 Codex 运行目录清单失败：{}", staging.display()))?;
        if let Err(error) = std::fs::rename(&staging, &destination) {
            if staged_runtime_ready(&destination, &sources) {
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
pub(crate) fn windows_cli_wrapper_target(app_dir: &std::path::Path) -> Result<PathBuf> {
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
    let marker_path = prepare_cli_wrapper_marker(&token).await;
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
        (
            crate::codex_startup_patch::CLI_WRAPPER_MARKER_ENV.to_string(),
            marker_path.to_string_lossy().to_string(),
        ),
    ];
    if crate::codex_startup_patch::local_router_runtime_enabled(runtime_config_overrides) {
        // Applies before Desktop chooses a transport, including CLI fallback
        // launches where the inspector patch cannot set this environment.
        environment.push(("CODEX_APP_SERVER_FORCE_CLI".to_string(), "1".to_string()));
    }
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
        marker_path,
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
        || error.is::<crate::codex_startup_patch::StartupProcessExited>()
        || error
            .downcast_ref::<std::io::Error>()
            .is_some_and(crate::codex_startup_patch::is_retryable_startup_io_error)
}

#[cfg(any(windows, test))]
fn should_retry_startup(error: &anyhow::Error, attempt: u32) -> bool {
    attempt < 2 && startup_error_allows_retry(error)
}

#[cfg(any(windows, all(target_os = "macos", test)))]
fn startup_launch_arguments(
    runtime_arguments: &[String],
    inspector_port: Option<u16>,
) -> Vec<String> {
    inspector_port
        .map(crate::codex_startup_patch::inspector_argument)
        .into_iter()
        .chain(runtime_arguments.iter().cloned())
        .collect()
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

/// Launches Codex with no compatibility entry at all. Only allowed when the
/// launch carries no runtime constraints; otherwise the caller must stop.
#[cfg(windows)]
async fn launch_windows_codex_without_compatibility(
    app_dir: &std::path::Path,
    debug_port: u16,
    runtime_arguments: &[String],
    runtime_config_overrides: &[String],
    subagent_gate_active: bool,
    startup_error: String,
) -> Result<SpawnedCodex> {
    if !runtime_config_overrides.is_empty() {
        anyhow::bail!(
            "Codex 启动兼容入口不可用，无法应用 app-server 运行时覆盖；为避免丢失 Codey 运行时约束，已停止启动：{startup_error}"
        );
    }
    if subagent_gate_active {
        anyhow::bail!(
            "Codex 启动兼容入口不可用；为避免丢失 Codey 运行时约束，已停止启动：{startup_error}"
        );
    }
    let (mut spawned, _, _) =
        spawn_windows_codex(app_dir, debug_port, runtime_arguments, &[], false)
            .await
            .with_context(|| format!("Codex 启动设置未能应用，且启动失败：{startup_error}"))?;
    spawned.performance_status = "degraded".to_string();
    spawned.performance_detail =
        "Codex 已启动，但部分启动设置未能应用；页面功能以检测结果为准，下次启动将重试".to_string();
    error_log::record_failure(
        "patch_degraded",
        "start_without_startup_patch",
        startup_error,
        serde_json::json!({
            "platform": "windows",
            "processId": spawned.process_id,
        }),
    );
    Ok(spawned)
}

#[cfg(any(windows, target_os = "macos"))]
async fn spawned_codex_alive(spawned: &mut SpawnedCodex) -> bool {
    if let Some(child) = spawned.child.as_mut() {
        return !matches!(child.try_wait(), Ok(Some(_)));
    }
    #[cfg(windows)]
    if let Some(process_id) = spawned.process_id {
        // Store activations hand back a PID without a child handle.
        return tokio::task::spawn_blocking(move || {
            codey_runtime_core::windows_enumerate_processes()
                .iter()
                .any(|process| process.process_id == process_id)
        })
        .await
        .unwrap_or(true);
    }
    true
}

/// Resolves once the launched process is gone; never resolves without one.
#[cfg(any(windows, target_os = "macos"))]
async fn startup_process_exited(
    spawned: Option<&mut SpawnedCodex>,
) -> crate::codex_startup_patch::StartupProcessExited {
    let Some(spawned) = spawned else {
        return std::future::pending().await;
    };
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.tick().await;
    loop {
        interval.tick().await;
        if !spawned_codex_alive(spawned).await {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.startup_process_exited",
                serde_json::json!({ "processId": spawned.process_id }),
            );
            return crate::codex_startup_patch::StartupProcessExited {
                process_id: spawned.process_id,
            };
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
async fn install_startup_patch_with_cli_fallback(
    inspector_port: Option<u16>,
    patch_options: crate::codex_startup_patch::PatchOptions,
    runtime_config_overrides: &[String],
    wrapper_handshake: Option<CliWrapperHandshake>,
    context: StartupWaitContext<'_>,
) -> Result<()> {
    let StartupWaitContext {
        platform,
        deadline,
        renderer_debug_port,
        spawned,
    } = context;
    let exited = startup_process_exited(spawned);
    let compatibility = wait_for_startup_compatibility(
        inspector_port,
        patch_options,
        runtime_config_overrides,
        wrapper_handshake,
        platform,
        deadline,
        renderer_debug_port,
    );
    tokio::select! {
        exited = exited => Err(exited.into()),
        result = compatibility => result,
    }
}

#[cfg(any(windows, target_os = "macos"))]
async fn wait_for_startup_compatibility(
    inspector_port: Option<u16>,
    patch_options: crate::codex_startup_patch::PatchOptions,
    runtime_config_overrides: &[String],
    wrapper_handshake: Option<CliWrapperHandshake>,
    platform: &'static str,
    deadline: tokio::time::Instant,
    renderer_debug_port: Option<u16>,
) -> Result<()> {
    use crate::codex_startup_patch::{
        CliWrapperFailure, InspectorUnavailable, loopback_port_accepts,
    };

    let Some(inspector_port) = inspector_port else {
        let handshake = wrapper_handshake.context(
            "Codex 启动兼容入口不可用：主进程 Inspector 已被 Electron fuse 关闭或本次不使用，且没有可用的 CLI 兼容入口",
        )?;
        return wait_for_cli_wrapper(handshake, deadline).await;
    };
    let mut patch_install = Box::pin(async {
        tokio::time::timeout_at(
            deadline,
            crate::codex_startup_patch::install(
                inspector_port,
                patch_options,
                runtime_config_overrides,
                !runtime_config_overrides.is_empty(),
                renderer_debug_port,
            ),
        )
        .await
        .context("Codex 兼容启动总时限已用尽")?
    });
    let Some(handshake) = wrapper_handshake else {
        return patch_install.as_mut().await;
    };
    let mut wrapper_ready = Box::pin(wait_for_cli_wrapper(handshake, deadline));
    tokio::select! {
        patch = &mut patch_install => match patch {
            Ok(()) => Ok(()),
            Err(patch_error) if patch_error.is::<InspectorUnavailable>() => {
                // The main process runs without an Inspector; only the CLI
                // wrapper can confirm the runtime configuration now, and it
                // keeps the whole readiness budget.
                let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                    "launcher.startup_compatibility_mode",
                    serde_json::json!({
                        "platform": platform,
                        "reason": "main_process_inspector_unavailable",
                        "inspectorPort": inspector_port,
                        "detail": format!("{patch_error:#}"),
                        "runtimeConfigOverrideCount": runtime_config_overrides.len(),
                    }),
                );
                wrapper_ready.as_mut().await
            }
            Err(patch_error) => {
                // Discovery timed out or the protocol failed. A live renderer
                // debug port proves the main script runs, so the wrapper may
                // still confirm; otherwise the main process is most likely
                // paused at `--inspect-brk` and waiting longer cannot help.
                let renderer_ready = match renderer_debug_port {
                    Some(debug_port) => loopback_port_accepts(debug_port).await,
                    None => true,
                };
                if !renderer_ready {
                    return Err(combined_startup_error(
                        patch_error,
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "渲染进程调试端口未就绪，主进程可能停在 --inspect-brk 断点，不再等待 CLI 兼容入口",
                        )
                        .into(),
                    ));
                }
                match wrapper_ready.as_mut().await {
                    Ok(()) => {
                        error_log::record_failure(
                            "patch_degraded",
                            "use_codex_cli_wrapper_after_patch_failure",
                            format!("{patch_error:#}"),
                            serde_json::json!({ "platform": platform }),
                        );
                        Ok(())
                    }
                    Err(wrapper_error) if wrapper_error.is::<CliWrapperFailure>() => Err(wrapper_error),
                    Err(wrapper_error) => Err(combined_startup_error(patch_error, wrapper_error)),
                }
            }
        },
        wrapper = &mut wrapper_ready => match wrapper {
            Ok(()) => {
                if loopback_port_accepts(inspector_port).await {
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
            Err(wrapper_error) if wrapper_error.is::<CliWrapperFailure>() => Err(wrapper_error),
            Err(wrapper_error) => match patch_install.as_mut().await {
                Ok(()) => Ok(()),
                Err(patch_error) => Err(combined_startup_error(patch_error, wrapper_error)),
            },
        },
    }
}

/// Polls the wrapper's marker file. Resolves on an executed or failed record;
/// keeps waiting while the file is missing or still says launching.
#[cfg(any(windows, target_os = "macos"))]
async fn watch_cli_wrapper_marker(path: &std::path::Path) -> Result<()> {
    use crate::codex_startup_patch::{CliWrapperFailure, CliWrapperMarker, CliWrapperMarkerStatus};

    let mut interval = tokio::time::interval(Duration::from_millis(250));
    let mut launching_logged = false;
    let mut invalid_logged = false;
    loop {
        interval.tick().await;
        match CliWrapperMarker::read(path) {
            Ok(None) => {}
            Ok(Some(marker)) => match marker.status {
                CliWrapperMarkerStatus::Launching => {
                    if !launching_logged {
                        launching_logged = true;
                        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                            "launcher.cli_wrapper_marker_seen",
                            serde_json::json!({ "wrapperPid": marker.pid }),
                        );
                    }
                }
                CliWrapperMarkerStatus::Executed => {
                    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                        "launcher.cli_wrapper_marker_confirmed",
                        serde_json::json!({ "wrapperPid": marker.pid }),
                    );
                    return Ok(());
                }
                CliWrapperMarkerStatus::Failed => {
                    return Err(CliWrapperFailure {
                        message: marker.message.unwrap_or_else(|| {
                            "目标程序未能执行，记录文件未包含失败详情".to_string()
                        }),
                        retryable: marker.retryable.unwrap_or(false),
                    }
                    .into());
                }
            },
            Err(error) => {
                if !invalid_logged {
                    invalid_logged = true;
                    error_log::record_failure(
                        "compatibility_fallback",
                        "read_cli_wrapper_marker",
                        format!("{error:#}"),
                        serde_json::json!({ "marker": path }),
                    );
                }
            }
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
async fn wait_for_cli_wrapper(
    handshake: CliWrapperHandshake,
    deadline: tokio::time::Instant,
) -> Result<()> {
    use crate::codex_startup_patch::{CliWrapperFailure, MAX_CLI_WRAPPER_FAILURE_BYTES};
    use tokio::io::AsyncReadExt;

    let CliWrapperHandshake {
        listener,
        token: expected_token,
        marker_path,
    } = handshake;
    let mut authenticated = false;
    let result = tokio::time::timeout_at(deadline, async {
        let accept_handshake = async {
            loop {
                let (mut stream, _) = listener.accept().await?;
                let mut received = vec![0; expected_token.len()];
                if tokio::time::timeout(Duration::from_millis(750), stream.read_exact(&mut received))
                    .await
                    .is_ok_and(|result| result.is_ok())
                    && received == expected_token
                {
                    authenticated = true;
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
        };
        let marker = watch_cli_wrapper_marker(&marker_path);
        tokio::select! {
            result = accept_handshake => result,
            result = marker => result,
        }
    })
    .await;
    let _ = std::fs::remove_file(&marker_path);
    result.with_context(|| {
        if authenticated {
            "Codex CLI 包装器已连接，但等待目标程序执行确认超时"
        } else {
            "等待 Codex CLI 兼容执行器超时：未收到有效的包装器握手，也没有执行记录"
        }
    })?
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

    #[cfg(any(windows, target_os = "macos"))]
    fn test_handshake(
        listener: tokio::net::TcpListener,
        token: &[u8],
    ) -> (CliWrapperHandshake, PathBuf) {
        let marker_path = std::env::temp_dir().join(format!(
            "codey-cli-wrapper-test-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        (
            CliWrapperHandshake {
                listener,
                token: token.to_vec(),
                marker_path: marker_path.clone(),
            },
            marker_path,
        )
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn test_context(deadline: tokio::time::Instant) -> StartupWaitContext<'static> {
        StartupWaitContext {
            platform: "windows",
            deadline,
            renderer_debug_port: None,
            spawned: None,
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    const TEST_PATCH_OPTIONS: crate::codex_startup_patch::PatchOptions =
        crate::codex_startup_patch::PatchOptions {
            disable_pet: false,
            subagent_gate_active: true,
        };

    #[tokio::test(start_paused = true)]
    async fn startup_retry_requires_a_transient_error_and_is_limited_to_two_attempts() {
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
        let exited: anyhow::Error = crate::codex_startup_patch::StartupProcessExited {
            process_id: Some(7),
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
        assert!(should_retry_startup(&timeout(), 1));
        assert!(should_retry_startup(&transient, 1));
        assert!(should_retry_startup(&exited, 1));
        assert!(!should_retry_startup(&invalid, 1));
        assert!(!should_retry_startup(&timeout(), 2));
        assert!(!startup_error_allows_retry(&combined_startup_error(
            anyhow::anyhow!("invalid inspector response"),
            timeout()
        )));
        assert!(startup_error_allows_retry(&combined_startup_error(
            timeout(),
            timeout()
        )));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(should_retry_startup(&timeout(), 1));
        assert!(!should_retry_startup(&timeout(), 2));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn compatibility_waits_share_the_callers_deadline() {
        let port = crate::codex_startup_patch::reserve_loopback_port().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let (handshake, _) = test_handshake(listener, b"token");
        let deadline = tokio::time::Instant::now() + Duration::from_millis(80);
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            install_startup_patch_with_cli_fallback(
                Some(port),
                TEST_PATCH_OPTIONS,
                &["analytics.enabled=false".to_string()],
                Some(handshake),
                test_context(deadline),
            ),
        )
        .await
        .expect("neither compatibility path may reset the caller's deadline");
        assert!(startup_error_allows_retry(&result.unwrap_err()));
        assert!(tokio::time::Instant::now() >= deadline);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn cli_retry_starts_without_a_breakpoint_and_gets_a_full_readiness_window() {
        use tokio::io::AsyncWriteExt;

        let runtime_args = vec!["--disable-gpu".to_string()];
        let port = crate::codex_startup_patch::reserve_loopback_port().unwrap();
        assert!(
            startup_launch_arguments(&runtime_args, Some(port))[0].starts_with("--inspect-brk=")
        );
        let first_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let (first_handshake, _) = test_handshake(first_listener, b"token");
        let overrides = ["model_provider=\"codey_router\"".to_string()];
        let error = install_startup_patch_with_cli_fallback(
            Some(port),
            TEST_PATCH_OPTIONS,
            &overrides,
            Some(first_handshake),
            test_context(tokio::time::Instant::now() + Duration::from_millis(40)),
        )
        .await
        .unwrap_err();
        assert!(should_retry_startup(&error, 1));

        // Simulate slow process cleanup, then start the new readiness window.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(startup_launch_arguments(&runtime_args, None), runtime_args);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (handshake, _) = test_handshake(listener, b"token");
        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(45)).await;
            tokio::time::resume();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(b"token").await.unwrap();
        });
        install_startup_patch_with_cli_fallback(
            None,
            TEST_PATCH_OPTIONS,
            &overrides,
            Some(handshake),
            test_context(
                tokio::time::Instant::now() + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
            ),
        )
        .await
        .unwrap();
        sender.await.unwrap();
        let error = install_startup_patch_with_cli_fallback(
            None,
            TEST_PATCH_OPTIONS,
            &overrides,
            None,
            test_context(tokio::time::Instant::now() + Duration::from_secs(1)),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("没有可用的 CLI 兼容入口"));
        assert!(!startup_error_allows_retry(&error));
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
            let (handshake, _) = test_handshake(listener, b"token");
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
                    Some(port),
                    TEST_PATCH_OPTIONS,
                    &[],
                    Some(handshake),
                    test_context(
                        tokio::time::Instant::now()
                            + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                    ),
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
            let (handshake, _) = test_handshake(listener, b"token");
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
                handshake,
                tokio::time::Instant::now() + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
            )
            .await;
            assert_eq!(result.is_err(), failed);
            sender.await.unwrap();
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn cli_wrapper_marker_confirms_execution_or_failure_without_a_connection() {
        use crate::codex_startup_patch::{
            CliWrapperFailure, CliWrapperMarker, CliWrapperMarkerStatus,
        };

        for failed in [false, true] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let (handshake, marker_path) = test_handshake(listener, b"token");
            let writer_path = marker_path.clone();
            let writer = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                CliWrapperMarker::new(CliWrapperMarkerStatus::Launching)
                    .write(&writer_path)
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                let mut marker = CliWrapperMarker::new(if failed {
                    CliWrapperMarkerStatus::Failed
                } else {
                    CliWrapperMarkerStatus::Executed
                });
                if failed {
                    marker.message = Some("运行文件暂时被占用".to_string());
                    marker.retryable = Some(true);
                }
                marker.write(&writer_path).unwrap();
            });
            let result = wait_for_cli_wrapper(
                handshake,
                tokio::time::Instant::now() + Duration::from_secs(5),
            )
            .await;
            writer.await.unwrap();
            if failed {
                let error = result.unwrap_err();
                let failure = error
                    .downcast_ref::<CliWrapperFailure>()
                    .expect("a failed marker must surface as a wrapper failure");
                assert!(failure.message.contains("被占用"));
                assert!(failure.retryable);
            } else {
                result.unwrap();
            }
            assert!(!marker_path.exists(), "the launcher removes its marker");
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn startup_wait_ends_as_soon_as_the_codex_process_exits() {
        #[cfg(target_os = "macos")]
        let mut command = tokio::process::Command::new("sleep");
        #[cfg(target_os = "macos")]
        command.arg("30");
        #[cfg(windows)]
        let mut command = tokio::process::Command::new("cmd");
        #[cfg(windows)]
        command.args(["/c", "ping -n 30 127.0.0.1 > NUL"]);
        let mut child = command.spawn().unwrap();
        let process_id = child.id();
        child.kill().await.unwrap();
        let mut spawned = SpawnedCodex {
            child: Some(child),
            process_id,
            #[cfg(unix)]
            process_group_id: process_id,
            #[cfg(target_os = "macos")]
            inspector_argument: None,
            performance_status: String::new(),
            performance_detail: String::new(),
        };
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let (handshake, marker_path) = test_handshake(listener, b"token");
        let started = std::time::Instant::now();
        let error = install_startup_patch_with_cli_fallback(
            None,
            TEST_PATCH_OPTIONS,
            &[],
            Some(handshake),
            StartupWaitContext {
                platform: "windows",
                deadline: tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                renderer_debug_port: None,
                spawned: Some(&mut spawned),
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.is::<crate::codex_startup_patch::StartupProcessExited>(),
            "{error:#}"
        );
        assert!(startup_error_allows_retry(&error));
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_file(marker_path);
    }

    #[test]
    fn stale_cli_wrapper_markers_are_pruned() {
        let temp = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now();
        let old = now - Duration::from_secs(2 * 60 * 60);
        let stale = temp.path().join("stale.json");
        let fresh = temp.path().join("fresh.json");
        let other = temp.path().join("stale.txt");
        for path in [&stale, &fresh, &other] {
            std::fs::write(path, "{}").unwrap();
        }
        for path in [&stale, &other] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        assert_eq!(
            prune_cli_wrapper_markers(temp.path(), now, CLI_WRAPPER_MARKER_MAX_AGE),
            1
        );
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(other.exists());
        assert_eq!(
            prune_cli_wrapper_markers(
                &temp.path().join("missing"),
                now,
                CLI_WRAPPER_MARKER_MAX_AGE
            ),
            0
        );
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
        let (handshake, _) = test_handshake(listener, b"token");
        let sender = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(18)).await;
            tokio::time::resume();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(b"token").await.unwrap();
        });
        install_startup_patch_with_cli_fallback(
            Some(inspector_port),
            TEST_PATCH_OPTIONS,
            &["analytics.enabled=false".to_string()],
            Some(handshake),
            test_context(
                tokio::time::Instant::now() + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
            ),
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
    fn windows_cli_runtime_is_staged_once_and_repaired_when_a_copy_is_damaged() {
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
        let staged_dir = staged.parent().unwrap().to_path_buf();
        assert!(staged.starts_with(local_app_data.join("OpenAI/Codex/bin")));
        let directory_name = staged_dir.file_name().unwrap().to_str().unwrap();
        assert_eq!(directory_name.len(), 16);
        assert!(directory_name.chars().all(|c| c.is_ascii_hexdigit()));
        for name in WINDOWS_CLI_RUNTIME_FILES {
            assert_eq!(
                std::fs::read(staged_dir.join(name)).unwrap(),
                format!("payload:{name}").as_bytes()
            );
        }
        let manifest: StagedRuntimeManifest = serde_json::from_slice(
            &std::fs::read(staged_dir.join(STAGED_RUNTIME_MANIFEST)).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.version, STAGED_RUNTIME_MANIFEST_VERSION);
        assert_eq!(manifest.files.len(), WINDOWS_CLI_RUNTIME_FILES.len());
        assert!(manifest.files.iter().all(|file| file.sha256.len() == 64));

        // Unchanged package files reuse the directory without rewriting it.
        std::fs::write(staged_dir.join("reused.marker"), "1").unwrap();
        assert_eq!(
            stage_windows_cli_runtime(&target, &local_app_data).unwrap(),
            staged
        );
        assert!(staged_dir.join("reused.marker").exists());

        // A damaged copy is detected by its size and staged again.
        std::fs::write(&staged, "truncated").unwrap();
        assert_eq!(
            stage_windows_cli_runtime(&target, &local_app_data).unwrap(),
            staged
        );
        assert_eq!(std::fs::read(&staged).unwrap(), b"payload:codex.exe");
        assert!(!staged_dir.join("reused.marker").exists());

        // A new package build gets its own directory.
        std::fs::write(&target, "payload:codex.exe v2").unwrap();
        let updated = stage_windows_cli_runtime(&target, &local_app_data).unwrap();
        assert_ne!(updated.parent(), staged.parent());
        assert_eq!(std::fs::read(&updated).unwrap(), b"payload:codex.exe v2");
    }
}
