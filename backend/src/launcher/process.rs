use super::*;
#[cfg(windows)]
use std::path::Path;

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
                            let mut interval = tokio::time::interval(Duration::from_secs(1));
                            loop {
                                tokio::select! {
                                    _ = &mut shutdown_rx => break false,
                                    _ = interval.tick() => {
                                        if process_probe_confirms_exit(
                                            codey_runtime_core::windows_process_is_running(process_id),
                                            Some(process_id),
                                        ) {
                                            break true;
                                        }
                                    }
                                }
                            }
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
    #[cfg(windows)]
    pub(super) startup_process: Option<WindowsStartupProcess>,
    #[cfg(unix)]
    pub(super) process_group_id: Option<u32>,
    #[cfg(target_os = "macos")]
    pub(super) inspector_argument: Option<String>,
    pub(super) performance_status: String,
    pub(super) performance_detail: String,
    /// Confirmed main-process injection path: `inspector`, `node_options`, `cli`,
    /// or empty when none of those completed.
    pub(super) startup_injection_mode: String,
}

/// How the main-process patch actually landed. CLI confirmation alone is not
/// Inspector / `--require`; those patches must not be claimed from a wrapper.
#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupInjectionMode {
    Inspector,
    NodeRequire,
    CliWrapper,
}

#[cfg(any(windows, target_os = "macos"))]
impl StartupInjectionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inspector => "inspector",
            Self::NodeRequire => "node_options",
            Self::CliWrapper => "cli",
        }
    }
}

/// A packaged Electron runtime may drop the inherited `NODE_OPTIONS`, which
/// leaves the `--require` patch unloaded and its marker file missing while the
/// fuse scan still reports the flag as enabled. A live renderer debug port is
/// the counterpart of the Inspector probe's early exit: the main script already
/// started, so the patch can no longer load and waiting out the readiness
/// budget only delays the switch to Inspector.
#[cfg(any(windows, target_os = "macos"))]
const RENDERER_READY_REQUIRE_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// First delay between renderer debug-port probes of a `--require` attempt;
/// later probes back off up to 500ms.
#[cfg(any(windows, target_os = "macos"))]
const RENDERER_READY_PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// One automatic fuse repair before the first Windows launch attempt.
///
/// The launcher restarts Codex itself, so a repaired runtime continues with
/// `--require` inside the same startup. Only installs the current user may write
/// are repaired; a protected runtime, a build that was already attempted or a
/// failure keeps the CLI compatibility mode and the manual repair button.
#[cfg(windows)]
async fn repair_main_process_injection_before_launch(
    app_dir: &Path,
    fuses: crate::electron_fuses::ElectronFuses,
) -> crate::electron_fuses::ElectronFuses {
    use crate::electron_fuses::AutoRepairOutcome;

    let repair_dir = app_dir.to_path_buf();
    let outcome = tokio::task::spawn_blocking(move || {
        crate::electron_fuses::auto_repair_node_options(&repair_dir)
    })
    .await
    .unwrap_or_else(|error| {
        AutoRepairOutcome::Failed(format!("主进程注入自动修复任务异常退出：{error}"))
    });
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.main_process_injection_auto_repair",
        serde_json::json!({
            "appPath": app_dir,
            "outcome": outcome.as_str(),
            "error": outcome.error(),
        }),
    );
    if let Some(error) = outcome.error() {
        error_log::record_failure(
            "runtime_repair_failed",
            "auto_repair_main_process_injection",
            error.to_string(),
            serde_json::json!({ "platform": "windows", "appPath": app_dir }),
        );
    }
    if !outcome.repaired() {
        return fuses;
    }
    crate::electron_fuses::detect_electron_fuses(app_dir.to_path_buf()).await
}

/// Whether this app directory is launched through Microsoft Store activation.
///
/// Such a launch is activated over COM rather than started as a child process,
/// so the runtime may drop the environment Codey would otherwise inherit, and
/// its package files are protected against fuse repair. `NODE_OPTIONS` is
/// therefore not a usable main-process entry there, while Inspector arrives
/// with the activation arguments.
///
/// Windows only: macOS starts a `.app` through `open -n`, which passes the
/// launch environment explicitly, so a Mac App Store copy needs no separate
/// channel decision. Never call this from the macOS branch.
#[cfg(any(windows, test))]
fn windows_app_dir_supports_packaged_activation(app_dir: &std::path::Path) -> bool {
    codey_runtime_core::app_paths::packaged_app_user_model_id(app_dir).is_some()
}

#[cfg(any(windows, test))]
fn windows_should_repair_main_process_injection(
    attempt: u32,
    packaged_activation: bool,
    fuses: crate::electron_fuses::ElectronFuses,
) -> bool {
    // A Store package is a protected copy, so repairing it would neither
    // succeed nor make `NODE_OPTIONS` survive the activation; it only adds a
    // failed write and a possible elevation prompt to the first attempt.
    attempt == 1
        && !packaged_activation
        && !fuses.node_options.node_options_possible()
        && !fuses.node_cli_inspect.inspector_possible()
}

/// Whether this attempt should prepare the `NODE_OPTIONS --require` payload.
///
/// A Store activation cannot observe Codey's environment, so `NODE_OPTIONS` is
/// dropped and its marker never arrives; the entry is only worth preparing when
/// Inspector is unavailable and `NODE_OPTIONS` is the last main-process entry
/// left. Standalone installs keep the existing preference.
#[cfg(any(windows, test))]
fn windows_should_prepare_require_patch(
    packaged_activation: bool,
    inspect_fuse: crate::electron_fuses::FuseState,
    options_fuse: crate::electron_fuses::FuseState,
    retry_without_require: bool,
) -> bool {
    if retry_without_require || !options_fuse.node_options_possible() {
        return false;
    }
    !packaged_activation || !inspect_fuse.inspector_possible()
}

#[cfg_attr(
    not(windows),
    allow(clippy::ptr_arg, reason = "Windows 启动重试需要替换调用方的应用目录")
)]
pub(super) async fn spawn_codex(
    app_dir: &mut PathBuf,
    debug_port: u16,
    disable_codex_pet: bool,
    subagent_gate_active: bool,
    misc_model: Option<String>,
    gpu_launch_mode: GpuLaunchMode,
    runtime_config_overrides: &[String],
) -> Result<SpawnedCodex> {
    #[cfg(any(windows, target_os = "macos"))]
    let patch_options = crate::codex_startup_patch::PatchOptions {
        disable_pet: disable_codex_pet,
        subagent_gate_active,
        misc_model,
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = (
        disable_codex_pet,
        subagent_gate_active,
        misc_model,
        runtime_config_overrides,
    );
    let runtime_arguments =
        codex_runtime_arguments(gpu_launch_mode, !cfg!(target_os = "macos"), cfg!(windows));

    #[cfg(windows)]
    {
        // Prefer NODE_OPTIONS `--require` whenever that fuse is on. Inspector
        // evaluate is the fallback. Never combine the two: both wrap Module._load.
        let mut retry_without_inspector = false;
        // Electron drops every NODE_OPTIONS flag except --max-http-header-size
        // and --http-parser in packaged apps, so `--require` may never write
        // its marker. A retry then gives Inspector its turn.
        let mut retry_without_require = false;
        let mut attempt = 0;
        loop {
            attempt += 1;
            *app_dir = refresh_windows_packaged_app_dir(app_dir)?;
            error_log::refresh_codex_app_version(Some(app_dir), None);
            // A Store activation sets the launch environment through the package
            // debug settings and cannot be observed from Codey's own environment,
            // so the first attempt must not spend the readiness budget on a
            // channel the runtime drops silently. Inspector carries the launch
            // arguments the activation interface does accept, which is why it is
            // preferred here instead of `NODE_OPTIONS`. Both entries stay fused
            // as Electron ships them; if an update turns the inspect flags off,
            // `NODE_OPTIONS` remains the only main-process entry left.
            let packaged_activation = windows_app_dir_supports_packaged_activation(app_dir);
            let mut fuses =
                crate::electron_fuses::detect_electron_fuses(app_dir.to_path_buf()).await;
            if windows_should_repair_main_process_injection(attempt, packaged_activation, fuses) {
                // Both main-process entries are off, so the launcher would fall
                // back to the CLI wrapper. Repairing the fuse byte now reuses the
                // restart this launch already performs; the manual repair stays
                // for installs that need administrator rights.
                fuses = repair_main_process_injection_before_launch(app_dir, fuses).await;
            }
            let inspect_fuse = fuses.node_cli_inspect;
            let require_wanted = windows_should_prepare_require_patch(
                packaged_activation,
                inspect_fuse,
                fuses.node_options,
                retry_without_require,
            );
            let require_patch = prepare_startup_require_launch(
                require_wanted,
                patch_options.clone(),
                runtime_config_overrides,
                "windows",
            )
            .await;
            let use_require = require_patch.is_some();
            let use_inspector =
                !use_require && inspect_fuse.inspector_possible() && !retry_without_inspector;

            let (wrapper, wrapper_preparation_error) = match prepare_cli_wrapper(
                app_dir,
                subagent_gate_active,
                runtime_config_overrides,
                use_inspector || use_require,
            )
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
            let inspector_port = if use_inspector {
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
            } else {
                None
            };
            if inspector_port.is_none() && wrapper.is_none() && require_patch.is_none() {
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
                        "启动尝试 {attempt}/2：NODE_OPTIONS 注入不可用（{}），主进程 Inspector 不可用（{}），且 CLI 兼容入口不可用：{error:#}",
                        fuses.node_options.as_str(),
                        inspect_fuse.as_str()
                    ),
                )
                .await;
            }
            let launch_arguments = startup_launch_arguments(&runtime_arguments, inspector_port);
            let mut launch_environment = require_patch
                .as_ref()
                .map(|prepared| prepared.environment.clone())
                .unwrap_or_default();
            if let Some(wrapper) = &wrapper {
                launch_environment.extend(wrapper.environment.iter().cloned());
            }
            // `--require` always needs the merged launch environment. Without
            // Inspector the wrapper is otherwise the only entry, so a constrained
            // launch must not proceed unless Store accepts it.
            let constrained = !runtime_config_overrides.is_empty() || subagent_gate_active;
            let launch = spawn_windows_codex(
                app_dir,
                debug_port,
                &launch_arguments,
                &launch_environment,
                use_require || (!use_inspector && constrained),
            )
            .await;
            let (mut spawned, package_debug_session, wrapper_environment_applied) = match launch {
                Ok(launch) => launch,
                Err(error) => {
                    let retry = should_retry_startup(&error, attempt);
                    error_log::record_failure(
                        "launch_failed",
                        "spawn_windows_codex",
                        format!("启动尝试 {attempt}/2：{error:#}"),
                        serde_json::json!({ "startupAttempt": attempt, "retryable": retry }),
                    );
                    if retry {
                        if !error.is::<WindowsPackageChanged>() {
                            retry_without_inspector = true;
                        }
                        continue;
                    }
                    return Err(error).context(format!("启动尝试 {attempt}/2：启动 Codex 失败"));
                }
            };
            // Each attempt gets its own readiness budget. Cleanup and Store
            // activation must not consume the next attempt's window.
            let deadline =
                tokio::time::Instant::now() + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT;
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_startup_attempt",
                serde_json::json!({
                    "attempt": attempt,
                    "useInspector": use_inspector,
                    "useRequire": use_require,
                    "packagedActivation": packaged_activation,
                    "inspectorFuse": inspect_fuse.as_str(),
                    "nodeOptionsFuse": fuses.node_options.as_str(),
                    "requirePrepared": require_patch.is_some(),
                    "processId": spawned.process_id,
                    "wrapperEnvironmentApplied": wrapper_environment_applied,
                }),
            );
            let wrapper_handshake = wrapper_environment_applied
                .then_some(wrapper)
                .flatten()
                .map(CliWrapperLaunch::into_handshake);
            let require_marker = wrapper_environment_applied
                .then_some(require_patch)
                .flatten()
                .map(|prepared| prepared.marker_path);
            if inspector_port.is_none() && wrapper_handshake.is_none() && require_marker.is_none() {
                // The process is already running without any compatibility
                // entry. It carries no constraints (see above), so keep it
                // instead of stopping and relaunching the same configuration.
                if let Some(session) = package_debug_session {
                    session
                        .finish()
                        .context("Windows Store Codex 兼容环境清理失败")?;
                }
                let startup_error = format!(
                    "启动尝试 {attempt}/2：未能应用主进程注入环境（Inspector {}，NODE_OPTIONS {}），且 Windows 未能应用 CLI 兼容环境，详见启动错误日志",
                    inspect_fuse.as_str(),
                    fuses.node_options.as_str()
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
                patch_options.clone(),
                runtime_config_overrides,
                wrapper_handshake,
                StartupWaitContext {
                    platform: "windows",
                    deadline,
                    renderer_debug_port: Some(debug_port),
                    spawned: Some(&mut spawned),
                    require_marker,
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
                Ok(mode) => {
                    spawned.startup_injection_mode = mode.as_str().to_string();
                    spawned.performance_status = "ready".to_string();
                    spawned.performance_detail = "Codex 启动成功".to_string();
                    return Ok(spawned);
                }
                Err(error) => {
                    let retryable = startup_error_allows_retry(&error);
                    // Exit code 0 during the wait is Electron's single-instance
                    // handoff: a Codex that Codey did not launch holds the lock.
                    let single_instance_exit = error
                        .downcast_ref::<crate::codex_startup_patch::StartupProcessExited>()
                        .is_some_and(|exited| exited.exit_code == Some(0));
                    let mut startup_error = format!("启动尝试 {attempt}/2：{error:#}");
                    error_log::record_failure(
                        "patch_failed",
                        "install_startup_patch_or_cli_wrapper",
                        startup_error.clone(),
                        serde_json::json!({
                            "platform": "windows",
                            "inspectorPort": inspector_port,
                            "inspectorFuse": inspect_fuse.as_str(),
                            "nodeOptionsFuse": fuses.node_options.as_str(),
                            "processId": spawned.process_id,
                            "startupAttempt": attempt,
                            "processes": windows_startup_process_details(app_dir, spawned.process_id),
                            "useInspector": use_inspector,
                            "useRequire": use_require,
                            "retryable": retryable,
                            "singleInstanceExitSuspected": single_instance_exit,
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
                    if single_instance_exit {
                        // The instance holding the lock is not the one just
                        // stopped; sweep every Codex install before retrying.
                        match stop_running_windows_codex_instances(app_dir).await {
                            Ok(instances) if instances.is_empty() => startup_error.push_str(
                                "；退出码 0 通常表示已有 Codex 实例占用了单实例锁，但未检测到其他 Codex 进程，请在任务管理器中结束所有 Codex 进程后重试",
                            ),
                            Ok(instances) => startup_error.push_str(&format!(
                                "；已停止占用单实例锁的其他 Codex 实例：{}",
                                windows_codex_instances_summary(&instances)
                            )),
                            Err(sweep_error) => startup_error.push_str(&format!(
                                "；退出码 0 通常表示已有 Codex 实例占用了单实例锁，且未能停止：{sweep_error:#}"
                            )),
                        }
                    }
                    // A main process paused at an unreachable `--inspect-brk`,
                    // a lost handshake or an early exit all get one more attempt
                    // through the other main-process entry (or the wrapper
                    // alone); the wrapper is prepared again.
                    if should_retry_startup(&error, attempt) {
                        if use_inspector {
                            retry_without_inspector = true;
                        }
                        if use_require {
                            retry_without_require = true;
                        }
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
        let fuses = crate::electron_fuses::detect_electron_fuses(app_dir.to_path_buf()).await;
        let inspect_fuse = fuses.node_cli_inspect;
        let is_app_bundle = app_dir.extension().and_then(|value| value.to_str()) == Some("app");
        let require_wanted = is_app_bundle && fuses.node_options.node_options_possible();
        let require_patch = prepare_startup_require_launch(
            require_wanted,
            patch_options.clone(),
            runtime_config_overrides,
            "macos",
        )
        .await;
        let use_require = require_patch.is_some();
        let use_inspector = !use_require && inspect_fuse.inspector_possible();
        // Pass `--inspect-brk` when using Inspector, or as a cleanup marker that
        // Electron drops when the inspect fuse is off. Never pass it together
        // with NODE_OPTIONS `--require` while inspect is on: both wrap Module._load.
        let pass_inspect_brk = use_inspector || !inspect_fuse.inspector_possible();
        let inspector_port = if pass_inspect_brk {
            Some(
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
                })?,
            )
        } else {
            None
        };
        let inspector_arg = inspector_port.map(crate::codex_startup_patch::inspector_argument);
        let launch_arguments = startup_launch_arguments(&runtime_arguments, inspector_port);
        let mut command = if is_app_bundle {
            build_fresh_macos_open_command(app_dir, debug_port, &launch_arguments)
        } else {
            build_codex_command(app_dir, debug_port, &launch_arguments)
        };
        let wrapper = if is_app_bundle {
            let wrapper = prepare_cli_wrapper(
                app_dir,
                subagent_gate_active,
                runtime_config_overrides,
                use_inspector || require_patch.is_some(),
            )
            .await?;
            add_macos_cli_wrapper(&mut command, &wrapper.environment)?;
            if let Some(require) = &require_patch {
                add_macos_cli_wrapper(&mut command, &require.environment)?;
            }
            Some(wrapper)
        } else {
            None
        };
        let mut spawned = spawn_command(command)?;
        spawned.inspector_argument = inspector_arg.clone();
        let wait_inspector_port = if use_inspector { inspector_port } else { None };
        let startup_result = install_startup_patch_with_cli_fallback(
            wait_inspector_port,
            patch_options.clone(),
            runtime_config_overrides,
            wrapper.map(CliWrapperLaunch::into_handshake),
            StartupWaitContext {
                platform: "macos",
                deadline: tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                renderer_debug_port: Some(debug_port),
                spawned: Some(&mut spawned),
                require_marker: require_patch.map(|prepared| prepared.marker_path),
            },
        )
        .await;

        match startup_result {
            Ok(mode) => {
                spawned.startup_injection_mode = mode.as_str().to_string();
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
                        "nodeOptionsFuse": fuses.node_options.as_str(),
                        "processId": spawned.process_id,
                        "processGroupId": spawned.process_group_id,
                        "disablePet": patch_options.disable_pet,
                    }),
                );
                let stop_result = match inspector_arg.as_deref() {
                    Some(inspector_arg) => {
                        stop_macos_codex(
                            inspector_arg,
                            app_dir,
                            spawned.process_id,
                            spawned.process_group_id,
                        )
                        .await
                    }
                    None => terminate_unix_codex_processes(
                        app_dir,
                        spawned.process_id,
                        spawned.process_group_id,
                        None,
                    )
                    .await
                    .map(|_| ()),
                };
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
    /// Marker written by the NODE_OPTIONS `--require` patch after it installs.
    require_marker: Option<PathBuf>,
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

/// Windows `MoveFileEx` reports access denied when the destination directory
/// already exists, and sharing or lock violations while a scanner still has a
/// newly copied executable open. Those are the failures that leave a verified
/// staging directory unpublished.
#[cfg(any(windows, test))]
fn windows_runtime_publish_retryable(error: &std::io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(any(windows, test))]
fn remove_windows_runtime_destination(destination: &std::path::Path) -> Result<()> {
    // `exists` / `is_dir` collapse a permission error into "missing", which
    // then turns into a bare access-denied rename. Surface the real status.
    match std::fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(destination).with_context(|| {
                format!(
                    "清理不完整的 Codex 用户运行目录失败：{}",
                    destination.display()
                )
            })
        }
        Ok(_) => std::fs::remove_file(destination).with_context(|| {
            format!(
                "清理无效的 Codex 用户运行路径失败：{}",
                destination.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("检查 Codex 用户运行目录失败：{}", destination.display())),
    }
}

/// Prefer the stable hash directory, then a previous fallback `{hash}-*` copy.
/// In-flight `.staging-*` directories are excluded so a concurrent publish can
/// still rename its own copy.
#[cfg(any(windows, test))]
fn find_ready_windows_runtime(
    cache_root: &std::path::Path,
    destination: &std::path::Path,
    hash_name: &str,
    sources: &[WindowsCliRuntimeSource],
) -> Option<std::path::PathBuf> {
    if staged_runtime_ready(destination, sources) {
        return Some(destination.to_path_buf());
    }
    let entries = std::fs::read_dir(cache_root).ok()?;
    let prefix = format!("{hash_name}-");
    for entry in entries.flatten().take(1024) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        if staged_runtime_ready(&path, sources) {
            return Some(path);
        }
    }
    None
}

#[cfg(any(windows, test))]
fn prune_abandoned_runtime_staging(
    cache_root: &std::path::Path,
    sources: &[WindowsCliRuntimeSource],
) {
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten().take(1024) {
        let file_name = entry.file_name();
        if !file_name
            .to_str()
            .is_some_and(|name| name.starts_with(".staging-"))
        {
            continue;
        }
        let path = entry.path();
        if staged_runtime_ready(&path, sources) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= Duration::from_secs(60 * 60));
        if stale {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(any(windows, test))]
fn publish_windows_staged_runtime(
    staging: &std::path::Path,
    destination: &std::path::Path,
    hash_name: &str,
    sources: &[WindowsCliRuntimeSource],
) -> Result<PathBuf> {
    const ATTEMPTS: u32 = 4;
    let mut last_error = None;
    for attempt in 1..=ATTEMPTS {
        if staged_runtime_ready(destination, sources) {
            let _ = std::fs::remove_dir_all(staging);
            return Ok(destination.join("codex.exe"));
        }
        if let Err(error) = remove_windows_runtime_destination(destination) {
            last_error = Some(error);
        }
        match std::fs::rename(staging, destination) {
            Ok(()) => return Ok(destination.join("codex.exe")),
            Err(error) => {
                if staged_runtime_ready(destination, sources) {
                    let _ = std::fs::remove_dir_all(staging);
                    return Ok(destination.join("codex.exe"));
                }
                let retryable = windows_runtime_publish_retryable(&error);
                let publish_error = anyhow::Error::from(error).context(format!(
                    "启用 Codex 用户运行目录失败：{}",
                    destination.display()
                ));
                // Keep the cleanup failure in the chain. A locked or unreadable
                // directory is why the rename was attempted against an occupant.
                last_error = Some(match last_error.take() {
                    Some(remove_error) => publish_error.context(format!("{remove_error:#}")),
                    None => publish_error,
                });
                if retryable && attempt < ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(50 * u64::from(attempt)));
                    continue;
                }
                break;
            }
        }
    }

    // The canonical directory can be locked by a running copy or an ACL that
    // hides it from `metadata`. The staging tree is already verified, so launch
    // from a sibling instead of dropping every runtime constraint.
    if staged_runtime_ready(staging, sources) {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let fallback = destination.with_file_name(format!("{hash_name}-{}", &suffix[..8]));
        let runtime_dir = if std::fs::rename(staging, &fallback).is_ok() {
            fallback
        } else {
            staging.to_path_buf()
        };
        #[cfg(not(test))]
        {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_runtime_publish_fallback",
                serde_json::json!({
                    "destination": destination,
                    "runtimeDir": runtime_dir,
                    "detail": last_error
                        .as_ref()
                        .map(|error| format!("{error:#}"))
                        .unwrap_or_default(),
                }),
            );
        }
        return Ok(runtime_dir.join("codex.exe"));
    }

    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!(
            "启用 Codex 用户运行目录失败：{}。请退出 Codex 后删除该目录并重新启动",
            destination.display()
        )
    }))
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
    let hash_name = &cache_hash[..16];
    // Codex 会清理自身 bin 中的旧哈希目录，Codey 的运行副本必须独立存放。
    let cache_root = local_app_data.join("Codey").join("codex-runtime");
    let destination = cache_root.join(hash_name);
    if let Some(ready) = find_ready_windows_runtime(&cache_root, &destination, hash_name, &sources)
    {
        return Ok(ready.join("codex.exe"));
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
    prune_abandoned_runtime_staging(&cache_root, &sources);

    let staging = cache_root.join(format!(
        ".staging-{hash_name}-{}",
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
        publish_windows_staged_runtime(&staging, &destination, hash_name, &sources)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(any(windows, test))]
fn windows_local_app_data(value: Option<std::ffi::OsString>) -> Result<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        // 浏览器辅助进程可能过滤 LOCALAPPDATA；Windows 已知文件夹查询不依赖该变量。
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().to_path_buf()))
        .context("无法定位 Windows 本地应用数据目录，无法准备 Codex 用户运行目录")
}

#[cfg(any(windows, test))]
pub(crate) fn windows_cli_wrapper_target(app_dir: &std::path::Path) -> Result<PathBuf> {
    let target =
        codey_runtime_core::app_paths::codex_runtime_executable(app_dir).ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                codey_runtime_core::app_paths::codex_runtime_executable_missing(app_dir)
            )
        })?;
    windows_cli_wrapper_target_from_source(app_dir, &target)
}

#[cfg(any(windows, test))]
fn windows_cli_wrapper_target_from_source(
    app_dir: &std::path::Path,
    target: &std::path::Path,
) -> Result<PathBuf> {
    if codey_runtime_core::app_paths::packaged_app_user_model_id(app_dir).is_none() {
        return Ok(target.to_path_buf());
    }
    let local_app_data = windows_local_app_data(std::env::var_os("LOCALAPPDATA"))?;
    stage_windows_cli_runtime(target, &local_app_data)
}

#[cfg(any(windows, target_os = "macos"))]
async fn prepare_startup_require_launch(
    enabled: bool,
    options: crate::codex_startup_patch::PatchOptions,
    runtime_config_overrides: &[String],
    platform: &'static str,
) -> Option<crate::codex_startup_patch::StartupRequire> {
    if !enabled {
        return None;
    }
    let overrides = runtime_config_overrides.to_vec();
    let prepared = tokio::task::spawn_blocking(move || {
        crate::codex_startup_patch::prepare_startup_require(options, &overrides)
    })
    .await;
    match prepared {
        Ok(Ok(prepared)) => Some(prepared),
        Ok(Err(error)) => {
            error_log::record_failure(
                "compatibility_fallback",
                "prepare_startup_require",
                format!("{error:#}"),
                serde_json::json!({ "platform": platform }),
            );
            None
        }
        Err(error) => {
            error_log::record_failure(
                "compatibility_fallback",
                "prepare_startup_require",
                format!("{error:#}"),
                serde_json::json!({ "platform": platform }),
            );
            None
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
async fn prepare_cli_wrapper(
    app_dir: &std::path::Path,
    subagent_gate_active: bool,
    runtime_config_overrides: &[String],
    handshake_optional: bool,
) -> Result<CliWrapperLaunch> {
    let codey = std::env::current_exe().context("定位 Codey 兼容执行器失败")?;
    let source =
        codey_runtime_core::app_paths::codex_runtime_executable(app_dir).ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                codey_runtime_core::app_paths::codex_runtime_executable_missing(app_dir)
            )
        })?;
    #[cfg(windows)]
    let target = {
        let app_dir = app_dir.to_path_buf();
        let source = source.clone();
        tokio::task::spawn_blocking(move || {
            windows_cli_wrapper_target_from_source(&app_dir, &source)
        })
        .await
        .context("准备 Windows Codex 用户运行文件的任务异常退出")??
    };
    #[cfg(target_os = "macos")]
    let target = source.clone();
    validate_code_mode_host(&target)?;
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
            crate::codex_startup_patch::CLI_WRAPPER_SOURCE_ENV.to_string(),
            source.to_string_lossy().to_string(),
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
    if handshake_optional {
        environment.push((
            crate::codex_startup_patch::CLI_WRAPPER_HANDSHAKE_OPTIONAL_ENV.to_string(),
            "1".to_string(),
        ));
    }
    if crate::codex_startup_patch::local_router_runtime_enabled(runtime_config_overrides) {
        // Applies before Desktop chooses a transport, including CLI fallback
        // launches where the inspector patch cannot set this environment.
        environment.push(("CODEX_APP_SERVER_FORCE_CLI".to_string(), "1".to_string()));
        environment.extend(local_router_proxy_bypass_environment(
            runtime_config_overrides,
            std::env::var("NO_PROXY").ok().as_deref(),
            std::env::var("no_proxy").ok().as_deref(),
        ));
    }
    #[cfg(windows)]
    let wrapper = codey;
    #[cfg(target_os = "macos")]
    let wrapper = {
        let path = crate::config::default_config_path().with_file_name("codex-cli-wrapper");
        // The wrapper write fsyncs; keep it off the two-worker async runtime.
        let write_path = path.clone();
        let write_codey = codey.clone();
        let write_environment = environment.clone();
        tokio::task::spawn_blocking(move || {
            write_macos_cli_wrapper(&write_path, &write_codey, &write_environment)
        })
        .await
        .context("写入 macOS Codex CLI 兼容入口的任务异常退出")??;
        path
    };
    if crate::codex_startup_patch::local_router_runtime_enabled(runtime_config_overrides) {
        environment.push((
            crate::codex_startup_patch::CLI_WRAPPER_STDIN_RELAY_ENV.to_string(),
            wrapper.to_string_lossy().to_string(),
        ));
    }
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

/// Local routing terminates at Codey's loopback listener. Keep that hop out of
/// the user's system proxy while preserving every existing bypass rule. Windows
/// treats environment keys case-insensitively, so it receives one canonical key.
#[cfg(any(windows, target_os = "macos"))]
fn local_router_proxy_bypass_environment(
    runtime_config_overrides: &[String],
    no_proxy: Option<&str>,
    lowercase_no_proxy: Option<&str>,
) -> Vec<(String, String)> {
    if !crate::codex_startup_patch::local_router_runtime_enabled(runtime_config_overrides) {
        return Vec::new();
    }
    #[cfg(windows)]
    {
        vec![(
            "NO_PROXY".to_string(),
            merge_loopback_no_proxy(no_proxy.or(lowercase_no_proxy)),
        )]
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            ("NO_PROXY".to_string(), merge_loopback_no_proxy(no_proxy)),
            (
                "no_proxy".to_string(),
                merge_loopback_no_proxy(lowercase_no_proxy.or(no_proxy)),
            ),
        ]
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (no_proxy, lowercase_no_proxy);
        Vec::new()
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn merge_loopback_no_proxy(existing: Option<&str>) -> String {
    const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "::1"];
    let mut entries = existing
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for required in LOOPBACK {
        if !entries.iter().any(|entry| {
            entry
                .trim_matches(['[', ']'])
                .eq_ignore_ascii_case(required)
        }) {
            entries.push(required.to_string());
        }
    }
    entries.join(",")
}

#[cfg(any(windows, target_os = "macos", test))]
fn validate_code_mode_host(target: &std::path::Path) -> Result<()> {
    // Codex Desktop enables features.code_mode_host in its app-server arguments.
    let host = target.with_file_name(if cfg!(windows) {
        "codex-code-mode-host.exe"
    } else {
        "codex-code-mode-host"
    });
    let metadata = std::fs::metadata(&host).with_context(|| {
        format!(
            "Codex code-mode 宿主不可用：{}；请修复或更新 Codex 安装后，通过 Codey 重新启动",
            host.display()
        )
    })?;
    anyhow::ensure!(
        metadata.is_file(),
        "Codex code-mode 宿主路径不是文件：{}",
        host.display()
    );
    Ok(())
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
    #[cfg(any(windows, test))]
    if error.is::<WindowsPackageChanged>() {
        return true;
    }
    if let Some(failure) = error.downcast_ref::<crate::codex_startup_patch::CliWrapperFailure>() {
        return failure.retryable;
    }
    #[cfg(windows)]
    if let Some(error) = error.downcast_ref::<windows::core::Error>() {
        // Store activation reports HRESULTs rather than std::io::Error.
        return error.code() == windows::Win32::Foundation::E_APPLICATION_ACTIVATION_TIMED_OUT
            || [32, 33, 1460]
                .into_iter()
                .any(|code| error.code() == windows::core::HRESULT::from_win32(code));
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

#[cfg(any(windows, target_os = "macos"))]
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
async fn spawned_codex_alive(spawned: &mut SpawnedCodex) -> Result<bool> {
    if let Some(child) = spawned.child.as_mut() {
        return child
            .try_wait()
            .map(|status| status.is_none())
            .context("检测 Codex 子进程状态失败");
    }
    #[cfg(windows)]
    if let Some(process_id) = spawned.process_id {
        if let Some(process) = &spawned.startup_process {
            return process.exit_code().map(|code| code.is_none());
        }
        // If opening the activation handle failed, keep the conservative PID probe.
        return codey_runtime_core::windows_process_is_running(process_id);
    }
    Ok(true)
}

#[cfg(any(windows, target_os = "macos", test))]
fn process_probe_confirms_exit(probe: Result<bool>, process_id: Option<u32>) -> bool {
    match probe {
        Ok(running) => !running,
        Err(error) => {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.process_probe_failed",
                serde_json::json!({ "processId": process_id, "detail": format!("{error:#}") }),
            );
            false
        }
    }
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
        if process_probe_confirms_exit(spawned_codex_alive(spawned).await, spawned.process_id) {
            let exit_code = spawned.child.as_mut().and_then(|child| {
                child
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|status| status.code())
                    .map(|code| code as u32)
            });
            #[cfg(windows)]
            let exit_code = exit_code.or_else(|| {
                spawned
                    .startup_process
                    .as_ref()
                    .and_then(|process| process.exit_code().ok().flatten())
            });
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.startup_process_exited",
                serde_json::json!({
                    "processId": spawned.process_id,
                    "exitCode": exit_code,
                    "exitCodeHex": exit_code.map(|code| format!("0x{code:08X}")),
                }),
            );
            return crate::codex_startup_patch::StartupProcessExited {
                process_id: spawned.process_id,
                exit_code,
            };
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn cleanup_startup_require_files(marker_path: &std::path::Path, remove_script: bool) {
    let _ = std::fs::remove_file(marker_path);
    if remove_script {
        let _ = std::fs::remove_file(marker_path.with_extension("js"));
        if let Some(name) = marker_path.file_name() {
            // space_free_path may have copied `{token}.js` into the temp dir.
            let _ = std::fs::remove_file(std::env::temp_dir().join(name).with_extension("js"));
        }
    }
}

/// Waits for the NODE_OPTIONS `--require` patch to write its executed marker.
/// CLI confirmation without that marker is a fallback, not proof the main
/// process loaded the patch.
#[cfg(any(windows, target_os = "macos"))]
async fn wait_for_require_patch_with_cli_fallback(
    marker_path: PathBuf,
    wrapper_handshake: Option<CliWrapperHandshake>,
    deadline: tokio::time::Instant,
    renderer_debug_port: Option<u16>,
    platform: &'static str,
) -> Result<StartupInjectionMode> {
    use crate::codex_startup_patch::{
        CliWrapperFailure, CliWrapperMarker, CliWrapperMarkerStatus, loopback_port_accepts,
    };

    // Races the patch marker and the wrapper against the renderer becoming
    // observable. A live renderer without a marker proves the runtime dropped
    // `NODE_OPTIONS`, so the attempt must end at the grace period's end instead
    // of exhausting the whole readiness budget.
    let watch_path = marker_path.clone();
    let mut require_ready = Box::pin(async move {
        tokio::time::timeout_at(deadline, watch_cli_wrapper_marker(&watch_path))
            .await
            .context("等待 Codex 主进程启动补丁确认超时")?
    });
    // A launch without a renderer debug port keeps the original two-way wait:
    // the probe must never resolve on its own, or it would short-circuit the
    // marker and wrapper channels it is only meant to back up.
    let mut renderer_ready = Box::pin(async move {
        match renderer_debug_port {
            Some(debug_port) => renderer_ready_watch(debug_port, deadline).await,
            None => std::future::pending().await,
        }
    });
    let result = if let Some(handshake) = wrapper_handshake {
        let mut wrapper_ready = Box::pin(wait_for_cli_wrapper(handshake, deadline));
        tokio::select! {
            require = &mut require_ready => match require {
                Ok(()) => Ok(StartupInjectionMode::NodeRequire),
                Err(require_error) => match wrapper_ready.as_mut().await {
                    Ok(()) => {
                        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                            "launcher.startup_compatibility_mode",
                            serde_json::json!({
                                "platform": platform,
                                "reason": "main_process_require_unavailable",
                                "detail": format!("{require_error:#}"),
                            }),
                        );
                        Ok(StartupInjectionMode::CliWrapper)
                    }
                    Err(wrapper_error) if wrapper_error.is::<CliWrapperFailure>() => {
                        Err(wrapper_error)
                    }
                    Err(wrapper_error) => Err(combined_startup_error(require_error, wrapper_error)),
                }
            },
            wrapper = &mut wrapper_ready => match wrapper {
                Ok(()) => match CliWrapperMarker::read(&marker_path) {
                    Ok(Some(marker)) if marker.status == CliWrapperMarkerStatus::Executed => {
                        Ok(StartupInjectionMode::NodeRequire)
                    }
                    _ => {
                        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                            "launcher.startup_compatibility_mode",
                            serde_json::json!({
                                "platform": platform,
                                "reason": "main_process_require_unavailable",
                            }),
                        );
                        Ok(StartupInjectionMode::CliWrapper)
                    }
                },
                Err(wrapper_error) if wrapper_error.is::<CliWrapperFailure>() => {
                    match require_ready.as_mut().await {
                        Ok(()) => Ok(StartupInjectionMode::NodeRequire),
                        Err(_) => Err(wrapper_error),
                    }
                }
                Err(wrapper_error) => match require_ready.as_mut().await {
                    Ok(()) => Ok(StartupInjectionMode::NodeRequire),
                    Err(require_error) => Err(combined_startup_error(require_error, wrapper_error)),
                }
            },
            _ = &mut renderer_ready => {
                // The grace period covers the marker write of a runtime that
                // does honour `NODE_OPTIONS`; the marker future above wins that
                // race by being polled first.
                Err(require_renderer_ready_error(&marker_path, platform))
            },
        }
    } else {
        tokio::select! {
            require = &mut require_ready => match require {
                Ok(()) => Ok(StartupInjectionMode::NodeRequire),
                Err(require_error) => {
                    let renderer_ready = match renderer_debug_port {
                        Some(debug_port) => loopback_port_accepts(debug_port).await,
                        None => false,
                    };
                    if renderer_ready {
                        Err(anyhow::anyhow!(
                            "Codex 主进程启动补丁未确认：渲染进程已就绪，但 NODE_OPTIONS --require 未写入执行记录：{require_error:#}"
                        ))
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("主进程启动补丁未确认，渲染进程调试端口未就绪：{require_error:#}"),
                        )
                        .into())
                    }
                }
            },
            _ = &mut renderer_ready => {
                Err(require_renderer_ready_error(&marker_path, platform))
            },
        }
    };
    let remove_script = CliWrapperMarker::read(&marker_path)
        .ok()
        .flatten()
        .is_some_and(|marker| marker.status == CliWrapperMarkerStatus::Executed);
    cleanup_startup_require_files(&marker_path, remove_script);
    result
}

/// Watches the renderer debug port of a `NODE_OPTIONS --require` attempt.
///
/// Resolves once the port has answered and the grace period passed without the
/// patch marker. The failure is deliberately retryable: the caller switches to
/// Inspector instead of waiting out the readiness budget.
#[cfg(any(windows, target_os = "macos"))]
async fn renderer_ready_watch(debug_port: u16, deadline: tokio::time::Instant) {
    let mut delay = RENDERER_READY_PROBE_INTERVAL;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let probe = tokio::time::timeout(
            remaining,
            crate::codex_startup_patch::loopback_port_accepts(debug_port),
        );
        let ready = match probe.await {
            Ok(ready) => ready,
            // Out of budget: the marker future reports the timeout instead.
            Err(_) => return,
        };
        if ready {
            let grace = deadline.saturating_duration_since(tokio::time::Instant::now());
            if !grace.is_zero() {
                tokio::time::sleep(grace.min(RENDERER_READY_REQUIRE_GRACE_PERIOD)).await;
            }
            return;
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay.saturating_mul(2), Duration::from_millis(500));
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn require_renderer_ready_error(
    marker_path: &std::path::Path,
    platform: &'static str,
) -> anyhow::Error {
    let detail = format!(
        "渲染进程已可观测，但 {} 未写入执行记录；该运行时丢弃了 NODE_OPTIONS，本轮按失败处理并切换主进程注入通道",
        marker_path.display()
    );
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.require_patch_renderer_ready",
        serde_json::json!({ "platform": platform, "markerPath": marker_path }),
    );
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("等待 Codex 主进程启动补丁确认超时: {detail}"),
    )
    .into()
}

#[cfg(any(windows, target_os = "macos"))]
async fn install_startup_patch_with_cli_fallback(
    inspector_port: Option<u16>,
    patch_options: crate::codex_startup_patch::PatchOptions,
    runtime_config_overrides: &[String],
    wrapper_handshake: Option<CliWrapperHandshake>,
    context: StartupWaitContext<'_>,
) -> Result<StartupInjectionMode> {
    let StartupWaitContext {
        platform,
        deadline,
        renderer_debug_port,
        spawned,
        require_marker,
    } = context;
    let exited = startup_process_exited(spawned);
    let compatibility = wait_for_startup_compatibility(
        inspector_port,
        patch_options,
        runtime_config_overrides,
        wrapper_handshake,
        require_marker,
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
#[allow(clippy::too_many_arguments)]
async fn wait_for_startup_compatibility(
    inspector_port: Option<u16>,
    patch_options: crate::codex_startup_patch::PatchOptions,
    runtime_config_overrides: &[String],
    wrapper_handshake: Option<CliWrapperHandshake>,
    require_marker: Option<PathBuf>,
    platform: &'static str,
    deadline: tokio::time::Instant,
    renderer_debug_port: Option<u16>,
) -> Result<StartupInjectionMode> {
    use crate::codex_startup_patch::{
        CliWrapperFailure, InspectorUnavailable, loopback_port_accepts,
    };

    if let Some(marker_path) = require_marker {
        return wait_for_require_patch_with_cli_fallback(
            marker_path,
            wrapper_handshake,
            deadline,
            renderer_debug_port,
            platform,
        )
        .await;
    }
    let Some(inspector_port) = inspector_port else {
        let handshake = wrapper_handshake.context(
            "Codex 启动兼容入口不可用：主进程 Inspector 与 NODE_OPTIONS --require 均不可用，且没有可用的 CLI 兼容入口",
        )?;
        return wait_for_cli_wrapper(handshake, deadline)
            .await
            .map(|_| StartupInjectionMode::CliWrapper);
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
        return patch_install
            .as_mut()
            .await
            .map(|_| StartupInjectionMode::Inspector);
    };
    let mut wrapper_ready = Box::pin(wait_for_cli_wrapper(handshake, deadline));
    tokio::select! {
        patch = &mut patch_install => match patch {
            Ok(()) => Ok(StartupInjectionMode::Inspector),
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
                wrapper_ready
                    .as_mut()
                    .await
                    .map(|_| StartupInjectionMode::CliWrapper)
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
                        Ok(StartupInjectionMode::CliWrapper)
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
                        Ok(()) => Ok(StartupInjectionMode::Inspector),
                        Err(patch_error) => {
                            error_log::record_failure(
                                "patch_degraded",
                                "use_codex_cli_wrapper_after_patch_failure",
                                format!("{patch_error:#}"),
                                serde_json::json!({ "platform": platform }),
                            );
                            Ok(StartupInjectionMode::CliWrapper)
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
                    Ok(StartupInjectionMode::CliWrapper)
                }
            },
            Err(wrapper_error) if wrapper_error.is::<CliWrapperFailure>() => Err(wrapper_error),
            Err(wrapper_error) => match patch_install.as_mut().await {
                Ok(()) => Ok(StartupInjectionMode::Inspector),
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

pub(super) async fn reap_owned_child_before_exit(child: &Mutex<Option<Child>>) -> Result<()> {
    let mut slot = child.lock().await;
    let Some(child) = slot.as_mut() else {
        return Ok(());
    };
    let process_id = child.id();
    if child
        .try_wait()
        .context("检查直属 Codex 子进程失败")?
        .is_none()
    {
        // 使用持有的 Child，不依赖可能失效的进程快照，也不扩大终止范围。
        child.start_kill().context("终止直属 Codex 子进程失败")?;
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .with_context(|| format!("等待直属 Codex 子进程退出超时：{process_id:?}"))?
            .context("回收直属 Codex 子进程失败")?;
    }
    slot.take();
    Ok(())
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
    let mut arguments = Vec::new();
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
        // Electron's single-instance lock is per app, not per install path: a
        // Codex left running from any directory (another install, a manual
        // start, a build that updated into a new folder) would make the launch
        // below quit with exit code 0, so every instance is stopped here.
        let stopped = stop_running_windows_codex_instances(app_dir)
            .await
            .context("停止正在运行的 Codex 失败")?;
        if !stopped.is_empty() {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_codex_instances_stopped",
                serde_json::json!({ "phase": "prepare_codex_for_launch", "count": stopped.len() }),
            );
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
        startup_injection_mode: String::new(),
    })
}

#[cfg(test)]
mod cli_wrapper_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn local_router_proxy_bypass_merges_loopback_entries_without_changing_direct_mode() {
        let inherited = Some("corp.internal,127.0.0.1");
        let router = vec!["model_provider=codey_router".to_string()];
        let direct = vec!["model_provider=openai".to_string()];

        let enabled = local_router_proxy_bypass_environment(&router, inherited, None);
        assert_eq!(
            enabled.len(),
            1,
            "Windows must not receive case-conflicting proxy variables"
        );
        assert_eq!(enabled[0].0, "NO_PROXY");
        assert_eq!(enabled[0].1, "corp.internal,127.0.0.1,localhost,::1");
        assert!(local_router_proxy_bypass_environment(&direct, inherited, None).is_empty());
    }

    // 【自动化测试】启动 - 查询失败不报告退出，真实退出仍立即识别
    #[test]
    fn process_probe_requires_confirmed_exit() {
        assert!(!process_probe_confirms_exit(Ok(true), Some(7)));
        assert!(!process_probe_confirms_exit(
            Err(anyhow::anyhow!("access denied")),
            Some(7)
        ));
        assert!(process_probe_confirms_exit(Ok(false), Some(7)));
    }

    #[test]
    fn activation_failure_retries_only_after_both_cleanup_steps_succeed() {
        for (process_stopped, environment_cleared) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            let cleanup = |ok| {
                if ok {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("cleanup failed"))
                }
            };
            let error = startup_activation_error_after_cleanup(
                std::io::Error::new(std::io::ErrorKind::TimedOut, "activation timed out").into(),
                cleanup(process_stopped),
                cleanup(environment_cleared),
            );
            assert_eq!(
                should_retry_startup(&error, 1),
                process_stopped && environment_cleared
            );
            assert!(!should_retry_startup(&error, 2));
            assert!(format!("{error:#}").contains("activation timed out"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn store_hresult_retry_classification_preserves_permanent_failures() {
        for (code, retryable) in [
            (32, true),
            (33, true),
            (1460, true),
            (5, false),
            (193, false),
        ] {
            let error = anyhow::Error::from(windows::core::Error::from_hresult(
                windows::core::HRESULT::from_win32(code),
            ))
            .context("Store activation failed");
            assert_eq!(should_retry_startup(&error, 1), retryable);
            assert!(!should_retry_startup(&error, 2));
        }
        let error = windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_APPLICATION_ACTIVATION_TIMED_OUT,
        )
        .into();
        assert!(should_retry_startup(&error, 1));
        assert!(!should_retry_startup(&error, 2));
    }

    #[test]
    fn startup_exit_message_preserves_native_exit_code() {
        let error = crate::codex_startup_patch::StartupProcessExited {
            process_id: Some(42),
            exit_code: Some(0xC0000005),
        };
        assert!(error.to_string().contains("PID 42"));
        assert!(error.to_string().contains("0xC0000005"));
    }

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
            require_marker: None,
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn test_patch_options() -> crate::codex_startup_patch::PatchOptions {
        crate::codex_startup_patch::PatchOptions {
            disable_pet: false,
            subagent_gate_active: true,
            misc_model: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn startup_retry_requires_a_transient_error_and_is_limited_to_two_attempts() {
        let updated = || anyhow::Error::new(WindowsPackageChanged);
        assert!(should_retry_startup(&updated(), 1));
        assert!(!should_retry_startup(&updated(), 2));
        for (stopped, cleared) in [
            (Err(anyhow::anyhow!("process still running")), Ok(())),
            (Ok(()), Err(anyhow::anyhow!("cleanup failed"))),
        ] {
            assert!(!should_retry_startup(
                &startup_activation_error_after_cleanup(updated(), stopped, cleared),
                1,
            ));
        }
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
            exit_code: Some(1),
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
        let session_timeout = crate::codex_startup_patch::startup_debug_session_timeout();
        assert!(startup_error_allows_retry(&session_timeout));
        assert!(should_retry_startup(&session_timeout, 1));
        assert!(!should_retry_startup(&session_timeout, 2));
        assert!(startup_error_allows_retry(&combined_startup_error(
            session_timeout,
            timeout()
        )));
        let confirmation_timeout =
            crate::codex_startup_patch::app_server_runtime_override_confirmation_error(
                &serde_json::json!({
                    "exception": {
                        "description": "codey-app-server-runtime-overrides-timeout 未观察到 app-server 启动调用"
                    }
                }),
            );
        assert!(startup_error_allows_retry(&combined_startup_error(
            confirmation_timeout,
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
                test_patch_options(),
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
            test_patch_options(),
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
            test_patch_options(),
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
            test_patch_options(),
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
                    test_patch_options(),
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
    async fn require_marker_confirms_main_process_patch_without_cli() {
        use crate::codex_startup_patch::{CliWrapperMarker, CliWrapperMarkerStatus};

        let marker_path = std::env::temp_dir().join(format!(
            "codey-startup-require-test-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let script_path = marker_path.with_extension("js");
        std::fs::write(&script_path, "0").unwrap();
        let writer_path = marker_path.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            CliWrapperMarker::new(CliWrapperMarkerStatus::Executed)
                .write(&writer_path)
                .unwrap();
        });
        let mode = install_startup_patch_with_cli_fallback(
            None,
            test_patch_options(),
            &[],
            None,
            StartupWaitContext {
                platform: "windows",
                deadline: tokio::time::Instant::now() + Duration::from_secs(5),
                renderer_debug_port: None,
                spawned: None,
                require_marker: Some(marker_path.clone()),
            },
        )
        .await
        .unwrap();
        assert_eq!(mode, StartupInjectionMode::NodeRequire);
        writer.await.unwrap();
        assert!(
            !marker_path.exists(),
            "the launcher removes a confirmed require marker"
        );
        assert!(
            !script_path.exists(),
            "the launcher removes the --require script after it has loaded"
        );
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn require_timeout_without_renderer_is_retryable() {
        let marker_path = std::env::temp_dir().join(format!(
            "codey-startup-require-timeout-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let error = install_startup_patch_with_cli_fallback(
            None,
            test_patch_options(),
            &[],
            None,
            StartupWaitContext {
                platform: "windows",
                deadline: tokio::time::Instant::now() + Duration::from_millis(40),
                renderer_debug_port: None,
                spawned: None,
                require_marker: Some(marker_path.clone()),
            },
        )
        .await
        .unwrap_err();
        assert!(startup_error_allows_retry(&error), "{error:#}");
        let _ = std::fs::remove_file(marker_path);
    }

    /// A runtime that drops `NODE_OPTIONS` renders normally while the `--require`
    /// marker never appears. The attempt must end shortly after the renderer
    /// becomes observable so the launcher can switch to Inspector inside the
    /// same startup instead of burning the whole budget.
    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn renderer_ready_without_require_marker_ends_the_attempt_early() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let renderer_port = listener.local_addr().unwrap().port();
        let marker_path = std::env::temp_dir().join(format!(
            "codey-startup-require-renderer-ready-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let started = tokio::time::Instant::now();
        let error = install_startup_patch_with_cli_fallback(
            None,
            test_patch_options(),
            &[],
            None,
            StartupWaitContext {
                platform: "windows",
                deadline: started + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                renderer_debug_port: Some(renderer_port),
                spawned: None,
                require_marker: Some(marker_path.clone()),
            },
        )
        .await
        .unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            elapsed < crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT / 2,
            "the attempt must not wait out the readiness budget: {elapsed:?}"
        );
        assert!(startup_error_allows_retry(&error), "{error:#}");
        assert!(
            format!("{error:#}").contains("等待 Codex 主进程启动补丁确认超时"),
            "{error:#}"
        );
        let _ = std::fs::remove_file(marker_path);
    }

    /// Store packages are activated over COM instead of started as a child
    /// process, which is what makes their `NODE_OPTIONS` unusable: the entry
    /// must go straight to Inspector rather than spend a whole round on it.
    #[test]
    fn packaged_activation_decides_the_main_process_entry() {
        use crate::electron_fuses::FuseState;

        assert!(windows_app_dir_supports_packaged_activation(
            std::path::Path::new(
                r"C:\Program Files\WindowsApps\OpenAI.Codex_26.915.4065.0_x64__2p2nqsd0c76g0\app"
            )
        ));
        assert!(!windows_app_dir_supports_packaged_activation(
            std::path::Path::new(r"C:\Users\tester\AppData\Local\Programs\Codex")
        ));
        assert!(!windows_should_prepare_require_patch(
            true,
            FuseState::Enabled,
            FuseState::Enabled,
            false
        ));
    }

    /// Both entries ship enabled, but an update may turn the inspect flags off.
    /// A Store package then has to fall back to `NODE_OPTIONS` instead of giving
    /// up on main-process injection.
    #[test]
    fn packaged_activation_keeps_require_when_inspector_is_gone() {
        use crate::electron_fuses::FuseState;

        assert!(windows_should_prepare_require_patch(
            true,
            FuseState::Disabled,
            FuseState::Enabled,
            false
        ));
        assert!(windows_should_prepare_require_patch(
            true,
            FuseState::Removed,
            FuseState::Unknown,
            false
        ));
        assert!(!windows_should_prepare_require_patch(
            true,
            FuseState::Disabled,
            FuseState::Disabled,
            false
        ));
        assert!(windows_should_prepare_require_patch(
            false,
            FuseState::Enabled,
            FuseState::Enabled,
            false
        ));
        assert!(!windows_should_prepare_require_patch(
            false,
            FuseState::Enabled,
            FuseState::Enabled,
            true
        ));
    }

    #[test]
    fn store_packages_skip_the_protected_fuse_repair() {
        use crate::electron_fuses::{ElectronFuses, FuseState};

        let blocked = |node_options, node_cli_inspect| ElectronFuses {
            node_cli_inspect,
            node_options,
        };
        assert!(windows_should_repair_main_process_injection(
            1,
            false,
            blocked(FuseState::Disabled, FuseState::Disabled)
        ));
        assert!(!windows_should_repair_main_process_injection(
            1,
            true,
            blocked(FuseState::Disabled, FuseState::Disabled)
        ));
        assert!(!windows_should_repair_main_process_injection(
            2,
            false,
            blocked(FuseState::Disabled, FuseState::Disabled)
        ));
        assert!(!windows_should_repair_main_process_injection(
            1,
            false,
            blocked(FuseState::Enabled, FuseState::Unknown)
        ));
    }

    /// A require marker that lands in time still wins over the renderer probe.
    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn require_marker_wins_the_race_against_the_renderer_probe() {
        use crate::codex_startup_patch::{CliWrapperMarker, CliWrapperMarkerStatus};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let renderer_port = listener.local_addr().unwrap().port();
        let marker_path = std::env::temp_dir().join(format!(
            "codey-startup-require-renderer-race-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let writer = marker_path.clone();
        let sender = tokio::spawn(async move {
            let marker = CliWrapperMarker::new(CliWrapperMarkerStatus::Executed);
            let _ = marker.write(&writer);
        });
        let mode = install_startup_patch_with_cli_fallback(
            None,
            test_patch_options(),
            &[],
            None,
            StartupWaitContext {
                platform: "windows",
                deadline: tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                renderer_debug_port: Some(renderer_port),
                spawned: None,
                require_marker: Some(marker_path.clone()),
            },
        )
        .await
        .unwrap();
        assert_eq!(mode, StartupInjectionMode::NodeRequire);
        sender.await.unwrap();
        let _ = std::fs::remove_file(marker_path);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn cli_success_without_require_marker_does_not_claim_main_process_patch() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (handshake, _) = test_handshake(listener, b"token");
        let marker_path = std::env::temp_dir().join(format!(
            "codey-startup-require-cli-fallback-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let script_path = marker_path.with_extension("js");
        std::fs::write(&script_path, "0").unwrap();
        let sender = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(b"token").await.unwrap();
        });
        let mode = install_startup_patch_with_cli_fallback(
            None,
            test_patch_options(),
            &[],
            Some(handshake),
            StartupWaitContext {
                platform: "windows",
                deadline: tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                renderer_debug_port: None,
                spawned: None,
                require_marker: Some(marker_path.clone()),
            },
        )
        .await
        .unwrap();
        assert_eq!(mode, StartupInjectionMode::CliWrapper);
        sender.await.unwrap();
        assert!(
            script_path.exists(),
            "CLI fallback must not delete a --require script that may still be loading"
        );
        let _ = std::fs::remove_file(script_path);
        let _ = std::fs::remove_file(marker_path);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn startup_wait_ends_as_soon_as_the_codex_process_exits() {
        #[cfg(target_os = "macos")]
        let mut command = tokio::process::Command::new("sh");
        #[cfg(target_os = "macos")]
        command.args(["-c", "exit 17"]);
        #[cfg(windows)]
        let mut command = tokio::process::Command::new("cmd");
        #[cfg(windows)]
        command.args(["/d", "/c", "exit /b 17"]);
        let mut child = command.spawn().unwrap();
        let process_id = child.id();
        child.wait().await.unwrap();
        let mut spawned = SpawnedCodex {
            child: Some(child),
            process_id,
            #[cfg(windows)]
            startup_process: None,
            #[cfg(unix)]
            process_group_id: process_id,
            #[cfg(target_os = "macos")]
            inspector_argument: None,
            performance_status: String::new(),
            performance_detail: String::new(),
            startup_injection_mode: String::new(),
        };
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let (handshake, marker_path) = test_handshake(listener, b"token");
        let started = std::time::Instant::now();
        let error = install_startup_patch_with_cli_fallback(
            None,
            test_patch_options(),
            &[],
            Some(handshake),
            StartupWaitContext {
                platform: "windows",
                deadline: tokio::time::Instant::now()
                    + crate::codex_startup_patch::STARTUP_CLI_READY_TIMEOUT,
                renderer_debug_port: None,
                spawned: Some(&mut spawned),
                require_marker: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.is::<crate::codex_startup_patch::StartupProcessExited>(),
            "{error:#}"
        );
        assert!(startup_error_allows_retry(&error));
        assert_eq!(
            error
                .downcast_ref::<crate::codex_startup_patch::StartupProcessExited>()
                .unwrap()
                .exit_code,
            Some(17),
        );
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
            test_patch_options(),
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
    fn windows_local_app_data_recovers_filtered_environment() {
        let system_directory = directories::BaseDirs::new().unwrap();
        for value in [None, Some("".into()), Some("relative-directory".into())] {
            assert_eq!(
                windows_local_app_data(value).unwrap(),
                system_directory.data_local_dir()
            );
        }
        let custom_directory = tempfile::tempdir().unwrap();
        assert_eq!(
            windows_local_app_data(Some(custom_directory.path().as_os_str().to_owned())).unwrap(),
            custom_directory.path()
        );
    }

    #[test]
    fn code_mode_host_is_checked_before_launch() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("codex");
        let host = target.with_file_name(if cfg!(windows) {
            "codex-code-mode-host.exe"
        } else {
            "codex-code-mode-host"
        });
        let error = validate_code_mode_host(&target).unwrap_err();
        assert!(format!("{error:#}").contains(&host.to_string_lossy().to_string()));
        std::fs::create_dir(&host).unwrap();
        assert!(validate_code_mode_host(&target).is_err());
        std::fs::remove_dir(&host).unwrap();
        std::fs::write(&host, "test host").unwrap();
        validate_code_mode_host(&target).unwrap();
    }

    #[test]
    fn windows_cli_runtime_survives_codex_cache_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let resources = temp.path().join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        for name in WINDOWS_CLI_RUNTIME_FILES {
            std::fs::write(resources.join(name), format!("payload:{name}")).unwrap();
        }
        let local_app_data = temp.path().join("local-app-data");
        let official_root = local_app_data.join("OpenAI/Codex/bin");
        let current_hash = "0000000000000000";
        let old_hash = "1111111111111111";
        for hash in [current_hash, old_hash] {
            let directory = official_root.join(hash);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("codex.exe"), "official runtime").unwrap();
        }
        let target = resources.join("codex.exe");
        let staged = stage_windows_cli_runtime(&target, &local_app_data).unwrap();
        let staged_dir = staged.parent().unwrap();
        std::fs::write(staged_dir.join("reused.marker"), "1").unwrap();

        // 模拟 Codex Desktop 清理自身 bin 下的旧哈希目录。
        for entry in std::fs::read_dir(&official_root).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_str().unwrap();
            if entry.file_type().unwrap().is_dir()
                && (name.starts_with(&format!(".staging-{current_hash}-"))
                    || (name != current_hash
                        && name.len() == 16
                        && name
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                        && entry.path().join("codex.exe").exists()))
            {
                std::fs::remove_dir_all(entry.path()).unwrap();
            }
        }
        assert!(!official_root.join(old_hash).exists());
        assert!(official_root.join(current_hash).join("codex.exe").is_file());
        for name in WINDOWS_CLI_RUNTIME_FILES {
            assert_eq!(
                std::fs::read(staged_dir.join(name)).unwrap(),
                format!("payload:{name}").as_bytes()
            );
        }
        assert!(staged_runtime_ready(
            staged_dir,
            &windows_cli_runtime_sources(&target).unwrap()
        ));
        assert_eq!(
            stage_windows_cli_runtime(&target, &local_app_data).unwrap(),
            staged
        );
        assert!(staged_dir.join("reused.marker").exists());
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
        assert!(staged.starts_with(local_app_data.join("Codey/codex-runtime")));
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

        // Missing helper executables must also invalidate the cached directory.
        for name in &WINDOWS_CLI_RUNTIME_FILES[1..] {
            std::fs::remove_file(staged_dir.join(name)).unwrap();
            assert_eq!(
                stage_windows_cli_runtime(&target, &local_app_data).unwrap(),
                staged
            );
            assert_eq!(
                std::fs::read(staged_dir.join(name)).unwrap(),
                format!("payload:{name}").as_bytes()
            );
        }

        // A new package build gets its own directory.
        std::fs::write(&target, "payload:codex.exe v2").unwrap();
        let updated = stage_windows_cli_runtime(&target, &local_app_data).unwrap();
        assert_ne!(updated.parent(), staged.parent());
        assert_eq!(std::fs::read(&updated).unwrap(), b"payload:codex.exe v2");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn windows_cli_runtime_falls_back_when_the_publish_directory_is_locked() {
        let temp = tempfile::tempdir().unwrap();
        let resources = temp.path().join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        for name in WINDOWS_CLI_RUNTIME_FILES {
            std::fs::write(resources.join(name), format!("payload:{name}")).unwrap();
        }
        let local_app_data = temp.path().join("local-app-data");
        let target = resources.join("codex.exe");
        let staged = stage_windows_cli_runtime(&target, &local_app_data).unwrap();
        let staged_dir = staged.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&staged_dir).unwrap();
        std::fs::create_dir(&staged_dir).unwrap();
        let locked = staged_dir.join("locked.txt");
        std::fs::write(&locked, "locked").unwrap();
        let status = std::process::Command::new("chflags")
            .arg("uchg")
            .arg(&locked)
            .status()
            .unwrap();
        assert!(status.success(), "chflags uchg failed");

        struct ClearImmutable(std::path::PathBuf);
        impl Drop for ClearImmutable {
            fn drop(&mut self) {
                let _ = std::process::Command::new("chflags")
                    .arg("nouchg")
                    .arg(&self.0)
                    .status();
            }
        }
        let _clear = ClearImmutable(locked);

        let fallback = stage_windows_cli_runtime(&target, &local_app_data).unwrap();
        assert_ne!(fallback.parent(), Some(staged_dir.as_path()));
        let fallback_name = fallback
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap();
        let canonical_name = staged_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap();
        assert!(
            fallback_name.starts_with(&format!("{canonical_name}-")),
            "{fallback_name}"
        );
        for name in WINDOWS_CLI_RUNTIME_FILES {
            assert_eq!(
                std::fs::read(fallback.parent().unwrap().join(name)).unwrap(),
                format!("payload:{name}").as_bytes()
            );
        }
        assert!(staged_dir.join("locked.txt").is_file());
        assert_eq!(
            stage_windows_cli_runtime(&target, &local_app_data).unwrap(),
            fallback
        );
    }
}
