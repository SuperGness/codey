use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result};
use codex_plus_core::app_paths::resolve_codex_app_dir_with_saved;
use codex_plus_core::launcher::build_codex_command;
use codex_plus_data::{ProviderSyncResult, ProviderSyncStatus};
use serde::Serialize;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};

use crate::cdp;
use crate::codex_config::{
    apply_runtime_provider_config, codex_home, ensure_global_model_provider,
    restore_runtime_provider_config,
};
use crate::config::CodeyConfig;
use crate::maintenance_lock;
use crate::model_catalog;
use crate::pet_slim_patch;
use crate::plugin_marketplace;
use crate::provider_lease;
use crate::session_index_cleanup::{self, SessionIndexCleanupReport};
use crate::startup_maintenance::{self, ProviderSyncPlan};
use crate::trace_log_guard;

const CDP_WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
const CDP_WATCHDOG_FAILURE_THRESHOLD: u8 = 2;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceStatus {
    pub session_status: String,
    pub session_detail: String,
    pub session_threads: usize,
    pub plugin_status: String,
    pub plugin_detail: String,
    pub performance_status: String,
    pub performance_detail: String,
}

pub struct CodeyRuntime {
    pub codex_app_path: PathBuf,
    pub maintenance: MaintenanceStatus,
    pub applied_config: CodeyConfig,
    child: Arc<Mutex<Option<Child>>>,
    #[cfg(windows)]
    process_id: Option<u32>,
    #[cfg(target_os = "macos")]
    inspector_argument: Option<String>,
    codex_exited: Arc<AtomicBool>,
    watchdog_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    watchdog_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    exit_watchdog_shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

impl CodeyRuntime {
    pub async fn start(
        config: &CodeyConfig,
        handler: codex_plus_core::bridge::BridgeHandler,
    ) -> Result<(Self, oneshot::Receiver<()>)> {
        let home = codex_home();
        let injection_scripts = cdp::prepare_injection_scripts(
            config.slim_codex_pet,
            config.slim_codex_voice,
            &config.user_scripts,
        );
        let trace_guard_home = home.clone();
        let disable_trace_log_writes = config.disable_trace_log_writes;
        let initial_trace_guard = tokio::task::spawn_blocking(move || {
            trace_log_guard::configure(&trace_guard_home, disable_trace_log_writes)
        });
        let original_provider = ensure_global_model_provider(&home)?;

        // Permanent maintenance runs before Codey creates the temporary
        // direct-provider lease. A lightweight header/SQLite validation normally
        // reuses the last successful provider sync; provider changes still
        // fall back to the complete rollout and SQLite repair.
        let maintenance_home = home.clone();
        match maintenance_lock::recover_stale_locks(&maintenance_home) {
            Ok(recovered) => {
                for path in recovered {
                    eprintln!("已清理陈旧维护锁：{}", path.display());
                }
            }
            Err(error) => eprintln!("清理陈旧维护锁失败：{error:#}"),
        }
        let maintenance_provider = original_provider.clone();
        let (provider_sync, index_cleanup) = tokio::task::spawn_blocking(move || {
            let provider_sync = match startup_maintenance::provider_sync_plan(
                &maintenance_home,
                &maintenance_provider,
            ) {
                Ok(ProviderSyncPlan::Cached) => {
                    startup_maintenance::cached_provider_sync_result(&maintenance_provider)
                }
                Ok(ProviderSyncPlan::Full) | Err(_) => {
                    let result = codex_plus_data::run_provider_sync_with_target(
                        Some(&maintenance_home),
                        Some(&maintenance_provider),
                    );
                    if result.status == ProviderSyncStatus::Synced
                        && result.skipped_locked_rollout_files.is_empty()
                        && let Err(error) =
                            startup_maintenance::record_provider_sync_success(&maintenance_provider)
                    {
                        eprintln!("保存 Provider 同步状态失败：{error:#}");
                    }
                    result
                }
            };
            // `session_index.jsonl` is also cleaned before spawn, while its
            // source snapshot is stable. The original file is backed up.
            let index_cleanup = session_index_cleanup::cleanup(&maintenance_home);
            (provider_sync, index_cleanup)
        })
        .await
        .context("启动前会话修复任务异常退出")?;
        let (session_status, session_detail, session_threads) =
            session_maintenance_summary(&provider_sync, &index_cleanup);
        initial_trace_guard
            .await
            .context("Trace 日志保护切换任务异常退出")??;

        let configured_app_path = config.codex_app_path.trim();
        let app_dir = resolve_codex_app_dir_with_saved(
            (!configured_app_path.is_empty())
                .then(|| PathBuf::from(configured_app_path))
                .as_deref(),
            None,
        )
        .ok_or_else(|| {
            if configured_app_path.is_empty() {
                anyhow::anyhow!("找不到 Codex App，请在 Codey 配置中填写路径")
            } else {
                anyhow::anyhow!(
                    "配置的 Codex App 路径无效或指向了 Codex CLI；请选择 Codex 桌面 App，不要选择 codex.exe 命令行程序"
                )
            }
        })?;
        prepare_codex_for_launch(&app_dir).await?;
        let current_profile = config
            .active_profile()
            .ok_or_else(|| anyhow::anyhow!("找不到当前 Codex 线路"))?;
        let official_provider = current_profile.cc_switch_read_only;
        let use_official_catalog = match model_catalog::refresh_for_provider(
            &home,
            official_provider,
            config.upstream_models(),
            config.selected_models(),
        ) {
            Ok(_) => true,
            Err(error) if model_catalog::is_available(&home) => {
                eprintln!("刷新官方账号模型目录失败，沿用上一份合法镜像：{error:#}");
                true
            }
            Err(error) => {
                eprintln!("刷新官方账号模型目录失败，临时使用 Codex 内置目录：{error:#}");
                false
            }
        };
        let default_model = model_catalog::selection_state(
            &home,
            official_provider,
            config.upstream_models(),
            config.selected_models(),
            config.default_model(),
        )
        .unwrap_or_default()
        .default_model;
        apply_runtime_provider_config(
            &home,
            &current_profile,
            &original_provider,
            use_official_catalog,
            (!default_model.is_empty()).then_some(default_model.as_str()),
            config.fast_context_tools,
            config.subagent_optimization,
        )?;

        let marketplace_home = home.clone();
        let marketplace_task = tokio::task::spawn_blocking(move || {
            plugin_marketplace::ensure_marketplaces(&marketplace_home)
        });
        let pet_home = home.clone();
        let slim_codex_pet = config.slim_codex_pet;
        let pet_task = tokio::task::spawn_blocking(move || {
            pet_slim_patch::configure(&pet_home, slim_codex_pet)
        });
        let (marketplace_result, pet_result) = tokio::join!(marketplace_task, pet_task);
        let (plugin_status, plugin_detail) = match marketplace_result {
            Ok(Ok(_)) => ("ready".to_string(), "插件市场已自动修复".to_string()),
            Ok(Err(error)) => ("error".to_string(), format!("插件修复失败：{error}")),
            Err(error) => (
                "error".to_string(),
                format!("插件修复任务异常退出：{error}"),
            ),
        };
        let debug_port = codex_plus_core::ports::select_packaged_codex_debug_port(9229);
        match pet_result {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                return Err(restore_runtime_config_after_error(
                    &home,
                    error.context("应用 Codex 宠物精简设置失败"),
                ));
            }
            Err(error) => {
                return Err(restore_runtime_config_after_error(
                    &home,
                    anyhow::Error::new(error).context("Codex 宠物精简设置任务异常退出"),
                ));
            }
        };
        let spawned = match spawn_codex(
            &app_dir,
            debug_port,
            config.slim_codex_pet,
            config.slim_codex_voice,
        )
        .await
        {
            Ok(spawned) => spawned,
            Err(error) => {
                return Err(restore_runtime_config_after_error(&home, error));
            }
        };
        let maintenance = MaintenanceStatus {
            session_status,
            session_detail,
            session_threads,
            plugin_status,
            plugin_detail,
            performance_status: spawned.performance_status.clone(),
            performance_detail: spawned.performance_detail.clone(),
        };
        #[cfg(target_os = "macos")]
        let inspector_argument = spawned.inspector_argument.clone();
        let child = Arc::new(Mutex::new(spawned.child));
        let injected_target =
            match cdp::retry_inject_with_scripts(debug_port, handler.clone(), &injection_scripts)
                .await
            {
                Ok(target) => target,
                Err(error) => {
                    if let Some(mut child) = child.lock().await.take() {
                        let _ = child.kill().await;
                    }
                    #[cfg(windows)]
                    if let Some(process_id) = spawned.process_id {
                        terminate_windows_process(process_id).await;
                    }
                    #[cfg(target_os = "macos")]
                    if let Some(inspector_argument) = spawned.inspector_argument.as_deref() {
                        if let Err(stop_error) =
                            stop_macos_codex(inspector_argument, &app_dir).await
                        {
                            eprintln!("Codex 注入失败后的进程清理失败：{stop_error:#}");
                        }
                    }
                    return Err(restore_runtime_config_after_error(&home, error));
                }
            };

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let watchdog_handler = handler.clone();
        let watchdog_debug_port = debug_port;
        let watchdog_injection_scripts = injection_scripts.clone();
        let watchdog_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(CDP_WATCHDOG_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            let mut target = injected_target;
            let mut consecutive_failures = 0u8;
            'watchdog: loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    _ = interval.tick() => {}
                }
                let healthy = tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break 'watchdog,
                    result = cdp::is_target_healthy(target.websocket_url()) => {
                        result.unwrap_or(false)
                    }
                };
                if !watchdog_should_reinject(&mut consecutive_failures, healthy) {
                    continue;
                }
                let reinjection = tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break 'watchdog,
                    result = cdp::retry_inject_with_scripts(
                        watchdog_debug_port,
                        watchdog_handler.clone(),
                        &watchdog_injection_scripts,
                    ) => result,
                };
                match reinjection {
                    Ok(reinjected) => {
                        let previous = std::mem::replace(&mut target, reinjected);
                        previous.close().await;
                        consecutive_failures = 0;
                    }
                    Err(error) => {
                        eprintln!("Codey CDP bridge 恢复失败：{error:#}");
                        consecutive_failures = CDP_WATCHDOG_FAILURE_THRESHOLD.saturating_sub(1);
                    }
                }
            }
            target.close().await;
        });
        let codex_exited = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        let (exit_watchdog_shutdown, codex_exit) =
            spawn_codex_exit_watcher(child.clone(), spawned.process_id, codex_exited.clone());
        #[cfg(not(windows))]
        let (exit_watchdog_shutdown, codex_exit) =
            spawn_codex_exit_watcher(child.clone(), codex_exited.clone());
        Ok((
            Self {
                codex_app_path: app_dir,
                maintenance,
                applied_config: config.clone(),
                child,
                #[cfg(windows)]
                process_id: spawned.process_id,
                #[cfg(target_os = "macos")]
                inspector_argument,
                codex_exited,
                watchdog_shutdown: Mutex::new(Some(shutdown_tx)),
                watchdog_task: Mutex::new(Some(watchdog_task)),
                exit_watchdog_shutdown: Mutex::new(Some(exit_watchdog_shutdown)),
            },
            codex_exit,
        ))
    }

    pub async fn stop(&self) -> Result<()> {
        if let Some(sender) = self.watchdog_shutdown.lock().await.take() {
            let _ = sender.send(());
        }
        let watchdog_task = self.watchdog_task.lock().await.take();
        if let Some(task) = watchdog_task {
            if let Err(error) = task.await {
                eprintln!("Codey CDP watchdog 关闭失败：{error}");
            }
        }
        if let Some(sender) = self.exit_watchdog_shutdown.lock().await.take() {
            let _ = sender.send(());
        }
        if !self.codex_exited.load(Ordering::Acquire) {
            #[cfg(target_os = "macos")]
            let (requested_macos_stop, macos_stop_error) =
                if let Some(inspector_argument) = self.inspector_argument.as_deref() {
                    (
                        true,
                        stop_macos_codex(inspector_argument, &self.codex_app_path)
                            .await
                            .err(),
                    )
                } else {
                    (false, None)
                };
            #[cfg(not(target_os = "macos"))]
            let requested_macos_stop = false;

            if let Some(mut child) = self.child.lock().await.take() {
                if requested_macos_stop {
                    if tokio::time::timeout(Duration::from_secs(5), child.wait())
                        .await
                        .is_err()
                    {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                } else {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
            #[cfg(windows)]
            if let Some(process_id) = self.process_id {
                terminate_windows_process(process_id).await;
            }
            #[cfg(target_os = "macos")]
            if let Some(error) = macos_stop_error {
                restore_runtime_config(&codex_home())?;
                return Err(error);
            }
        }
        restore_runtime_config(&codex_home())?;
        Ok(())
    }
}

fn watchdog_should_reinject(consecutive_failures: &mut u8, healthy: bool) -> bool {
    if healthy {
        *consecutive_failures = 0;
        return false;
    }
    *consecutive_failures = consecutive_failures.saturating_add(1);
    *consecutive_failures >= CDP_WATCHDOG_FAILURE_THRESHOLD
}

fn session_maintenance_summary(
    provider_sync: &ProviderSyncResult,
    index_cleanup: &Result<SessionIndexCleanupReport>,
) -> (String, String, usize) {
    let mut errors = Vec::new();
    if provider_sync.status != ProviderSyncStatus::Synced {
        errors.push(provider_sync.message.clone());
    }
    if !provider_sync.skipped_locked_rollout_files.is_empty() {
        errors.push(format!(
            "跳过 {} 个被占用的会话文件",
            provider_sync.skipped_locked_rollout_files.len()
        ));
    }
    let pruned_entries = match index_cleanup {
        Ok(report) => report.pruned_entries,
        Err(error) => {
            errors.push(format!("幽灵任务索引清理失败：{error}"));
            0
        }
    };
    let session_threads = provider_sync.changed_session_files;

    let mut detail = format!(
        "已同步到 {}：修复 {} 个会话文件，更新 {} 行数据库索引，清理 {} 条幽灵任务",
        provider_sync.target_provider,
        provider_sync.changed_session_files,
        provider_sync.sqlite_rows_updated,
        pruned_entries,
    );
    if provider_sync.encrypted_content_warning.is_some() {
        detail.push_str("；检测到跨 Provider 加密历史警告");
    }
    if !errors.is_empty() {
        detail.push_str("；");
        detail.push_str(&errors.join("；"));
    }
    let status = if errors.is_empty() { "ready" } else { "error" };
    (status.to_string(), detail, session_threads)
}

pub fn restore_previous_runtime_state(home: &std::path::Path) -> Result<()> {
    let provider_result = provider_lease::restore_legacy();
    let config_result = restore_runtime_provider_config(home);
    match (provider_result, config_result) {
        (Ok(_), Ok(_)) => Ok(()),
        (Err(provider), Ok(_)) => Err(provider).context("恢复会话 provider 失败"),
        (Ok(_), Err(config)) => Err(config).context("恢复 Codex 配置失败"),
        (Err(provider), Err(config)) => {
            anyhow::bail!("恢复会话 provider 失败：{provider}；恢复 Codex 配置也失败：{config}")
        }
    }
}

pub fn restore_runtime_config(home: &std::path::Path) -> Result<()> {
    restore_runtime_provider_config(home)
        .map(|_| ())
        .context("恢复 Codex 配置失败")
}

fn restore_runtime_config_after_error(
    home: &std::path::Path,
    error: anyhow::Error,
) -> anyhow::Error {
    match restore_runtime_config(home) {
        Ok(()) => error,
        Err(restore_error) => {
            anyhow::anyhow!("{error:#}；启动失败后恢复临时 Codex 配置也失败：{restore_error:#}")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildProcessState {
    Running,
    Exited,
    Untracked,
}

async fn child_process_state(child: &Arc<Mutex<Option<Child>>>) -> ChildProcessState {
    let mut slot = child.lock().await;
    let state = match slot.as_mut() {
        Some(process) => match process.try_wait() {
            Ok(Some(_)) | Err(_) => ChildProcessState::Exited,
            Ok(None) => ChildProcessState::Running,
        },
        None => ChildProcessState::Untracked,
    };
    if state == ChildProcessState::Exited {
        slot.take();
    }
    state
}

#[cfg(not(windows))]
fn spawn_codex_exit_watcher(
    child: Arc<Mutex<Option<Child>>>,
    codex_exited: Arc<AtomicBool>,
) -> (oneshot::Sender<()>, oneshot::Receiver<()>) {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let natural_exit = loop {
            tokio::select! {
                _ = &mut shutdown_rx => break false,
                _ = interval.tick() => match child_process_state(&child).await {
                    ChildProcessState::Running => {}
                    ChildProcessState::Exited => {
                        codex_exited.store(true, Ordering::Release);
                        break true;
                    }
                    ChildProcessState::Untracked => break false,
                }
            }
        };
        if natural_exit {
            let _ = exit_tx.send(());
        }
    });
    (shutdown_tx, exit_rx)
}

#[cfg(windows)]
fn spawn_codex_exit_watcher(
    child: Arc<Mutex<Option<Child>>>,
    process_id: Option<u32>,
    codex_exited: Arc<AtomicBool>,
) -> (oneshot::Sender<()>, oneshot::Receiver<()>) {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (exit_tx, exit_rx) = oneshot::channel();
    tokio::spawn(async move {
        let natural_exit = if let Some(process_id) = process_id {
            tokio::select! {
                _ = &mut shutdown_rx => false,
                result = codex_plus_core::launcher::wait_for_windows_process_id(process_id) => {
                    match result {
                        Ok(()) => true,
                        Err(error) => {
                            eprintln!("等待 Windows Codex 进程退出失败：{error:#}");
                            !codex_plus_core::windows_enumerate_processes()
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
    (shutdown_tx, exit_rx)
}

struct SpawnedCodex {
    child: Option<Child>,
    #[cfg(windows)]
    process_id: Option<u32>,
    #[cfg(target_os = "macos")]
    inspector_argument: Option<String>,
    performance_status: String,
    performance_detail: String,
}

async fn spawn_codex(
    app_dir: &std::path::Path,
    debug_port: u16,
    disable_codex_pet: bool,
    disable_codex_voice: bool,
) -> Result<SpawnedCodex> {
    let patch_options = crate::codex_startup_patch::PatchOptions {
        disable_pet: disable_codex_pet,
        disable_voice: disable_codex_voice,
    };

    #[cfg(windows)]
    {
        let inspector_port = crate::codex_startup_patch::reserve_loopback_port()
            .context("为 Codex 启动补丁选择本地调试端口失败")?;
        let inspector_arg = crate::codex_startup_patch::inspector_argument(inspector_port);
        let mut spawned = spawn_windows_codex(app_dir, debug_port, &[inspector_arg]).await?;
        match crate::codex_startup_patch::install(inspector_port, patch_options).await {
            Ok(()) => {
                spawned.performance_status = "ready".to_string();
                spawned.performance_detail = startup_patch_detail();
                return Ok(spawned);
            }
            Err(error) => {
                stop_windows_spawned_codex(&mut spawned).await;
                return Err(error)
                    .context("Codex 启动硬补丁未能安装；已停止 Codex，未降级为仅隐藏 UI");
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let inspector_port = crate::codex_startup_patch::reserve_loopback_port()
            .context("为 macOS Codex 启动补丁选择本地调试端口失败")?;
        let inspector_arg = crate::codex_startup_patch::inspector_argument(inspector_port);
        let command = if app_dir.extension().and_then(|value| value.to_str()) == Some("app") {
            build_fresh_macos_open_command(
                app_dir,
                debug_port,
                std::slice::from_ref(&inspector_arg),
            )
        } else {
            build_codex_command(app_dir, debug_port, std::slice::from_ref(&inspector_arg))
        };
        let mut spawned = spawn_command(command)?;
        spawned.inspector_argument = Some(inspector_arg.clone());
        match crate::codex_startup_patch::install(inspector_port, patch_options).await {
            Ok(()) => {
                spawned.performance_status = "ready".to_string();
                spawned.performance_detail = startup_patch_detail();
                return Ok(spawned);
            }
            Err(error) => {
                if let Some(mut child) = spawned.child.take() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                if let Err(stop_error) = stop_macos_codex(&inspector_arg, app_dir).await {
                    eprintln!("Codex 启动补丁失败后的进程清理失败：{stop_error:#}");
                }
                return Err(error)
                    .context("Codex 启动硬补丁未能安装；已停止 Codex，未降级为仅隐藏 UI");
            }
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let command = build_codex_command(app_dir, debug_port, &[]);
        let mut spawned = spawn_command(command)?;
        spawned.performance_status = "ready".to_string();
        spawned.performance_detail = if disable_codex_pet {
            "当前平台不支持宠物硬屏蔽启动补丁".to_string()
        } else if disable_codex_voice {
            "当前平台不支持语音硬屏蔽启动补丁".to_string()
        } else {
            "当前平台无需 macOS / Windows 启动补丁".to_string()
        };
        Ok(spawned)
    }
}

async fn prepare_codex_for_launch(app_dir: &std::path::Path) -> Result<()> {
    // Existing Codex instances must remain untouched. On macOS spawn_codex
    // uses `open -n`, so Codey gets a fresh debuggable instance while the
    // user's existing app and app-server continue running.
    #[cfg(windows)]
    {
        let executable = codex_plus_core::app_paths::build_codex_executable(app_dir);
        let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
        let executable = normalized_windows_path(&executable);
        let already_running = codex_plus_core::windows_enumerate_processes()
            .into_iter()
            .filter_map(|process| process.executable_path)
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
            .any(|path| normalized_windows_path(&path) == executable);
        if already_running {
            anyhow::bail!(
                "Codex 启动补丁必须在主进程运行前应用。请从系统托盘完全退出 Codex，再重新启动 Codey"
            );
        }
    }
    #[cfg(not(windows))]
    let _ = app_dir;
    #[cfg(target_os = "macos")]
    if macos_codex_is_running(app_dir).await? {
        anyhow::bail!(
            "Codex 启动优化必须在主进程运行前应用。请完全退出已有 Codex，再重新启动 Codey"
        );
    }
    Ok(())
}

fn startup_patch_detail() -> String {
    #[cfg(windows)]
    {
        "Windows 启动补丁已启用：WMI 周期采样、临时 WebView 残留和执行环境泄漏已修复".to_string()
    }
    #[cfg(not(windows))]
    {
        "启动补丁已启用：临时 WebView 和执行环境会自动回收".to_string()
    }
}

fn spawn_command(command: Vec<String>) -> Result<SpawnedCodex> {
    let executable = command
        .first()
        .ok_or_else(|| anyhow::anyhow!("Codex 启动命令为空"))?;
    let mut child_command = Command::new(executable);
    child_command.args(&command[1..]);
    #[cfg(windows)]
    {
        child_command.env_remove("WSL_DISTRO_NAME");
        child_command.creation_flags(codex_plus_core::windows_create_no_window());
    }
    let child = child_command
        .spawn()
        .with_context(|| format!("启动 Codex 失败：{executable}"))?;
    Ok(SpawnedCodex {
        child: Some(child),
        #[cfg(windows)]
        process_id: None,
        #[cfg(target_os = "macos")]
        inspector_argument: None,
        performance_status: String::new(),
        performance_detail: String::new(),
    })
}

#[cfg(windows)]
fn normalized_windows_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_ascii_lowercase()
}

#[cfg(windows)]
async fn spawn_windows_codex(
    app_dir: &std::path::Path,
    debug_port: u16,
    extra_args: &[String],
) -> Result<SpawnedCodex> {
    if let Some(activation) =
        codex_plus_core::launcher::build_packaged_activation(app_dir, debug_port, extra_args)
        && let codex_plus_core::launcher::CodexLaunch::PackagedActivation {
            app_user_model_id,
            arguments,
            ..
        } = activation
    {
        let process_id =
            codex_plus_core::launcher::activate_packaged_app(&app_user_model_id, &arguments)
                .await?;
        return Ok(SpawnedCodex {
            child: None,
            process_id: Some(process_id),
            performance_status: String::new(),
            performance_detail: String::new(),
        });
    }

    let command = build_codex_command(app_dir, debug_port, extra_args);
    let executable = command
        .first()
        .ok_or_else(|| anyhow::anyhow!("Codex 启动命令为空"))?;
    let mut child_command = Command::new(executable);
    child_command.args(&command[1..]);
    // A stale WSL_DISTRO_NAME inherited by the native Windows app makes
    // current Codex builds synchronously probe wsl.exe during startup.
    child_command.env_remove("WSL_DISTRO_NAME");
    child_command.creation_flags(codex_plus_core::windows_create_no_window());
    let child = child_command
        .spawn()
        .with_context(|| format!("启动 Codex 失败：{executable}"))?;
    Ok(SpawnedCodex {
        child: Some(child),
        process_id: None,
        performance_status: String::new(),
        performance_detail: String::new(),
    })
}

#[cfg(windows)]
async fn stop_windows_spawned_codex(spawned: &mut SpawnedCodex) {
    if let Some(mut child) = spawned.child.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    if let Some(process_id) = spawned.process_id.take() {
        terminate_windows_process(process_id).await;
    }
}

#[cfg(target_os = "macos")]
fn build_fresh_macos_open_command(
    app_dir: &std::path::Path,
    debug_port: u16,
    extra_args: &[String],
) -> Vec<String> {
    let mut command =
        codex_plus_core::launcher::build_macos_open_command(app_dir, debug_port, extra_args);
    if command.first().map(String::as_str) == Some("open")
        && !command.iter().any(|part| part == "-n" || part == "--new")
    {
        command.insert(1, "-n".to_string());
    }
    command
}

#[cfg(target_os = "macos")]
async fn stop_macos_codex(inspector_argument: &str, app_dir: &std::path::Path) -> Result<()> {
    let status = Command::new("pkill")
        .args(["-f", "--", inspector_argument])
        .status()
        .await
        .context("按本次启动参数停止 macOS Codex 失败")?;
    if !status.success() && status.code() != Some(1) {
        anyhow::bail!("按本次启动参数停止 macOS Codex 失败：pkill 返回 {status}");
    }
    if wait_for_macos_codex_exit(app_dir, Duration::from_secs(1)).await? {
        return Ok(());
    }

    let process_ids = macos_codex_process_ids(app_dir).await?;
    if !process_ids.is_empty() {
        let mut command = Command::new("kill");
        command.arg("-TERM");
        for process_id in process_ids {
            command.arg(process_id.to_string());
        }
        let _ = command.status().await;
    }
    if wait_for_macos_codex_exit(app_dir, Duration::from_secs(5)).await? {
        return Ok(());
    }
    anyhow::bail!("停止 macOS Codex 超时，目标主进程仍在运行")
}

#[cfg(target_os = "macos")]
async fn macos_codex_process_ids(app_dir: &std::path::Path) -> Result<Vec<u32>> {
    let executable = codex_plus_core::app_paths::build_codex_executable(app_dir);
    let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
    let executable = executable.to_string_lossy();
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .await
        .context("检查 macOS Codex 进程失败")?;
    if !output.status.success() {
        anyhow::bail!("检查 macOS Codex 进程失败：ps 返回 {}", output.status);
    }
    let processes = String::from_utf8_lossy(&output.stdout);
    Ok(processes
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let separator = line.find(char::is_whitespace)?;
            let (process_id, command) = line.split_at(separator);
            let command = command.trim_start();
            let matches = command == executable
                || command
                    .strip_prefix(executable.as_ref())
                    .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace));
            matches.then(|| process_id.parse::<u32>().ok()).flatten()
        })
        .collect())
}

#[cfg(target_os = "macos")]
async fn macos_codex_is_running(app_dir: &std::path::Path) -> Result<bool> {
    Ok(!macos_codex_process_ids(app_dir).await?.is_empty())
}

#[cfg(target_os = "macos")]
async fn wait_for_macos_codex_exit(app_dir: &std::path::Path, timeout: Duration) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !macos_codex_is_running(app_dir).await? {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(windows)]
async fn terminate_windows_process(process_id: u32) {
    let mut command = Command::new("taskkill");
    command
        .args(["/PID", &process_id.to_string(), "/T", "/F"])
        .creation_flags(codex_plus_core::windows_create_no_window());
    let _ = command.status().await;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_launch_forces_a_new_app_instance() {
        let command = build_fresh_macos_open_command(
            std::path::Path::new("/Applications/ChatGPT.app"),
            9229,
            &["--inspect-brk=127.0.0.1:19321".to_string()],
        );
        assert_eq!(command.first().map(String::as_str), Some("open"));
        assert!(command.iter().any(|part| part == "-n"));
        assert!(command.iter().any(|part| part == "-W"));
        assert!(
            command
                .iter()
                .any(|part| part == "--remote-debugging-port=9229")
        );
        assert!(
            command
                .iter()
                .any(|part| part == "--inspect-brk=127.0.0.1:19321")
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_running_check_does_not_match_an_unrelated_app_path() {
        let running = macos_codex_is_running(std::path::Path::new(
            "/Applications/Definitely Not Codex.app",
        ))
        .await
        .unwrap();
        assert!(!running);
    }

    #[tokio::test]
    async fn exit_watcher_reports_a_naturally_exited_child() {
        let child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn short-lived child");
        let child = Arc::new(Mutex::new(Some(child)));
        let exited = Arc::new(AtomicBool::new(false));
        let (_shutdown, exit_rx) = spawn_codex_exit_watcher(child, exited.clone());

        tokio::time::timeout(Duration::from_secs(2), exit_rx)
            .await
            .expect("watcher timed out")
            .expect("watcher was cancelled");
        assert!(exited.load(Ordering::Acquire));
    }

    #[test]
    fn cdp_watchdog_requires_consecutive_failures_before_reinjecting() {
        let mut failures = 0;

        assert!(!watchdog_should_reinject(&mut failures, false));
        assert_eq!(failures, 1);
        assert!(!watchdog_should_reinject(&mut failures, true));
        assert_eq!(failures, 0);
        assert!(!watchdog_should_reinject(&mut failures, false));
        assert!(watchdog_should_reinject(&mut failures, false));
    }
}
