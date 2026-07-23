#[cfg(unix)]
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
#[cfg(any(unix, windows))]
use std::{collections::HashSet, path::Path};

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
use crate::config::{CodeyConfig, GpuLaunchMode};
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
pub const CODEX_APP_NOT_FOUND_ERROR: &str = "找不到 Codex App，请在 Codey 配置中填写路径";
pub const CODEX_APP_PATH_INVALID_ERROR: &str = "配置的 Codex App 路径无效或指向了 Codex CLI；请选择 Codex 桌面 App，不要选择 codex.exe 命令行程序";
const DISABLE_GPU_ARGUMENT: &str = "--disable-gpu";
const DISABLE_GPU_RASTERIZATION_ARGUMENT: &str = "--disable-gpu-rasterization";

#[cfg(windows)]
pub fn needs_codex_app_path_selection(startup_error: Option<&str>) -> bool {
    startup_error.is_some_and(|error| {
        error.contains(CODEX_APP_NOT_FOUND_ERROR) || error.contains(CODEX_APP_PATH_INVALID_ERROR)
    })
}

#[cfg(all(test, windows))]
mod app_path_selection_tests {
    use super::*;

    #[test]
    fn path_selection_is_requested_only_for_app_path_failures() {
        assert!(needs_codex_app_path_selection(Some(
            CODEX_APP_NOT_FOUND_ERROR
        )));
        assert!(needs_codex_app_path_selection(Some(
            CODEX_APP_PATH_INVALID_ERROR
        )));
        assert!(!needs_codex_app_path_selection(Some("网络不可用")));
        assert!(!needs_codex_app_path_selection(None));
    }
}

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
    process_id: Option<u32>,
    #[cfg(unix)]
    process_group_id: Option<u32>,
    #[cfg(target_os = "macos")]
    inspector_argument: Option<String>,
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
                anyhow::anyhow!(CODEX_APP_NOT_FOUND_ERROR)
            } else {
                anyhow::anyhow!(CODEX_APP_PATH_INVALID_ERROR)
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
            config.upstream_models_snapshot(),
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
            config.upstream_models_snapshot(),
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
            plugin_marketplace::marketplaces_status(&marketplace_home)
        });
        let pet_home = home.clone();
        let slim_codex_pet = config.slim_codex_pet;
        let pet_task = tokio::task::spawn_blocking(move || {
            pet_slim_patch::configure(&pet_home, slim_codex_pet)
        });
        let (marketplace_result, pet_result) = tokio::join!(marketplace_task, pet_task);
        let (plugin_status, plugin_detail) = match marketplace_result {
            Ok(status)
                if !status
                    .get("needsRepair")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true) =>
            {
                (
                    "ready".to_string(),
                    "插件市场状态正常；启动时未执行修复".to_string(),
                )
            }
            Ok(_) => (
                "needs_repair".to_string(),
                "插件市场需要修复；可在 Codey 配置页手动处理".to_string(),
            ),
            Err(error) => (
                "error".to_string(),
                format!("插件市场状态任务异常退出：{error}"),
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
            config.gpu_launch_mode,
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
                    #[cfg(windows)]
                    if let Err(stop_error) =
                        terminate_windows_codex_processes(&app_dir, spawned.process_id).await
                    {
                        eprintln!("Codex 注入失败后的进程清理失败：{stop_error:#}");
                    }
                    #[cfg(target_os = "macos")]
                    if let Some(inspector_argument) = spawned.inspector_argument.as_deref() {
                        if let Err(stop_error) = stop_macos_codex(
                            inspector_argument,
                            &app_dir,
                            spawned.process_id,
                            spawned.process_group_id,
                        )
                        .await
                        {
                            eprintln!("Codex 注入失败后的进程清理失败：{stop_error:#}");
                        }
                    }
                    #[cfg(all(unix, not(target_os = "macos")))]
                    if let Err(stop_error) = terminate_unix_codex_processes(
                        &app_dir,
                        spawned.process_id,
                        spawned.process_group_id,
                        None,
                    )
                    .await
                    {
                        eprintln!("Codex 注入失败后的进程清理失败：{stop_error:#}");
                    }
                    if let Some(mut child) = child.lock().await.take()
                        && tokio::time::timeout(Duration::from_secs(2), child.wait())
                            .await
                            .is_err()
                    {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
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
                process_id: spawned.process_id,
                #[cfg(unix)]
                process_group_id: spawned.process_group_id,
                #[cfg(target_os = "macos")]
                inspector_argument,
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
        #[cfg(target_os = "macos")]
        let process_stop = if let Some(inspector_argument) = self.inspector_argument.as_deref() {
            stop_macos_codex(
                inspector_argument,
                &self.codex_app_path,
                self.process_id,
                self.process_group_id,
            )
            .await
        } else {
            terminate_unix_codex_processes(
                &self.codex_app_path,
                self.process_id,
                self.process_group_id,
                None,
            )
            .await
            .map(|_| ())
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let process_stop = terminate_unix_codex_processes(
            &self.codex_app_path,
            self.process_id,
            self.process_group_id,
            None,
        )
        .await
        .map(|_| ());
        #[cfg(windows)]
        let process_stop =
            terminate_windows_codex_processes(&self.codex_app_path, self.process_id).await;
        #[cfg(not(any(unix, windows)))]
        let process_stop: Result<()> = Ok(());

        if let Some(mut child) = self.child.lock().await.take()
            && tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .is_err()
        {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let config_restore = restore_runtime_config(&codex_home());
        match (process_stop, config_restore) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(process_error), Ok(())) => Err(process_error),
            (Ok(()), Err(config_error)) => Err(config_error),
            (Err(process_error), Err(config_error)) => anyhow::bail!(
                "清理 Codex 遗留进程失败：{process_error:#}；恢复 Codex 配置也失败：{config_error:#}"
            ),
        }
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
    process_id: Option<u32>,
    #[cfg(unix)]
    process_group_id: Option<u32>,
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
    gpu_launch_mode: GpuLaunchMode,
) -> Result<SpawnedCodex> {
    let patch_options = crate::codex_startup_patch::PatchOptions {
        disable_pet: disable_codex_pet,
        disable_voice: disable_codex_voice,
    };
    let gpu_arguments = gpu_launch_arguments(gpu_launch_mode, !cfg!(target_os = "macos"));

    #[cfg(windows)]
    {
        let inspector_port = crate::codex_startup_patch::reserve_loopback_port()
            .context("为 Codex 启动补丁选择本地调试端口失败")?;
        let inspector_arg = crate::codex_startup_patch::inspector_argument(inspector_port);
        let mut launch_arguments = vec![inspector_arg];
        launch_arguments.extend(gpu_arguments);
        let mut spawned = spawn_windows_codex(app_dir, debug_port, &launch_arguments).await?;
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
        let mut launch_arguments = vec![inspector_arg.clone()];
        launch_arguments.extend(gpu_arguments);
        let command = if app_dir.extension().and_then(|value| value.to_str()) == Some("app") {
            build_fresh_macos_open_command(app_dir, debug_port, &launch_arguments)
        } else {
            build_codex_command(app_dir, debug_port, &launch_arguments)
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
                if let Err(stop_error) = stop_macos_codex(
                    &inspector_arg,
                    app_dir,
                    spawned.process_id,
                    spawned.process_group_id,
                )
                .await
                {
                    eprintln!("Codex 启动补丁失败后的进程清理失败：{stop_error:#}");
                }
                if let Some(mut child) = spawned.child.take()
                    && tokio::time::timeout(Duration::from_secs(2), child.wait())
                        .await
                        .is_err()
                {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                return Err(error)
                    .context("Codex 启动硬补丁未能安装；已停止 Codex，未降级为仅隐藏 UI");
            }
        }
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let command = build_codex_command(app_dir, debug_port, &gpu_arguments);
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

fn gpu_launch_arguments(gpu_launch_mode: GpuLaunchMode, enabled_for_platform: bool) -> Vec<String> {
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
    #[cfg(unix)]
    child_command.process_group(0);
    #[cfg(windows)]
    {
        child_command.env_remove("WSL_DISTRO_NAME");
        child_command.creation_flags(codex_plus_core::windows_create_no_window());
    }
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
    let process_id = child.id();
    Ok(SpawnedCodex {
        child: Some(child),
        process_id,
        performance_status: String::new(),
        performance_detail: String::new(),
    })
}

#[cfg(windows)]
async fn stop_windows_spawned_codex(spawned: &mut SpawnedCodex) {
    if let Some(process_id) = spawned.process_id.take() {
        if let Err(error) = terminate_windows_process(process_id).await {
            eprintln!("Codex 启动失败后的进程清理失败：{error:#}");
        }
    }
    if let Some(mut child) = spawned.child.take()
        && tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
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
async fn stop_macos_codex(
    inspector_argument: &str,
    app_dir: &std::path::Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
) -> Result<()> {
    terminate_unix_codex_processes(
        app_dir,
        process_id,
        process_group_id,
        Some(inspector_argument),
    )
    .await
    .map(|_| ())
}

#[cfg(unix)]
fn owned_unix_codex_process_ids(
    processes: &[crate::process_tree::UnixProcessInfo],
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> HashSet<u32> {
    let current_process_id = std::process::id();
    let roots = processes.iter().filter_map(|process| {
        let matches_root = Some(process.process_id) == process_id
            || Some(process.process_group_id) == process_group_id
            || crate::process_tree::command_uses_path(&process.command, app_dir)
            || launch_marker.is_some_and(|marker| {
                crate::process_tree::command_has_argument(&process.command, marker)
            });
        matches_root.then_some(process.process_id)
    });
    crate::process_tree::process_ids_with_descendants(processes, roots, current_process_id)
}

#[cfg(unix)]
fn owned_unix_process_group(
    processes: &[crate::process_tree::UnixProcessInfo],
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> Option<u32> {
    let process_group_id = process_group_id?;
    processes
        .iter()
        .any(|process| {
            process.process_group_id == process_group_id
                && (Some(process.process_id) == process_id
                    || crate::process_tree::command_uses_path(&process.command, app_dir)
                    || launch_marker.is_some_and(|marker| {
                        crate::process_tree::command_has_argument(&process.command, marker)
                    }))
        })
        .then_some(process_group_id)
}

#[cfg(unix)]
async fn terminate_unix_codex_processes(
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> Result<usize> {
    let mut known_processes = HashMap::new();
    let mut processes = crate::process_tree::unix_process_snapshot().await?;
    let initially_owned = owned_unix_codex_process_ids(
        &processes,
        app_dir,
        process_id,
        process_group_id,
        launch_marker,
    );
    known_processes.extend(crate::process_tree::identities_for_process_ids(
        &processes,
        &initially_owned,
    ));

    let owned_process_group = owned_unix_process_group(
        &processes,
        app_dir,
        process_id,
        process_group_id,
        launch_marker,
    );
    crate::process_tree::signal_process_group(owned_process_group, libc::SIGTERM)?;
    crate::process_tree::signal_processes(
        &known_processes.keys().copied().collect(),
        libc::SIGTERM,
    )?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let poll_delays = [
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_millis(350),
        Duration::from_millis(550),
        Duration::from_millis(800),
    ];
    let mut poll_index = 0usize;
    let remaining = loop {
        let currently_owned = owned_unix_codex_process_ids(
            &processes,
            app_dir,
            process_id,
            process_group_id,
            launch_marker,
        );
        let newly_discovered = currently_owned
            .into_iter()
            .filter(|process_id| !known_processes.contains_key(process_id))
            .collect::<HashSet<_>>();
        if !newly_discovered.is_empty() {
            crate::process_tree::signal_processes(&newly_discovered, libc::SIGTERM)?;
            known_processes.extend(crate::process_tree::identities_for_process_ids(
                &processes,
                &newly_discovered,
            ));
        }
        let remaining = crate::process_tree::matching_process_ids(&processes, &known_processes);
        if remaining.is_empty() || tokio::time::Instant::now() >= deadline {
            break remaining;
        }
        let remaining_time = deadline.saturating_duration_since(tokio::time::Instant::now());
        let delay = poll_delays
            .get(poll_index)
            .copied()
            .unwrap_or(Duration::from_millis(800))
            .min(remaining_time);
        poll_index = poll_index.saturating_add(1);
        tokio::time::sleep(delay).await;
        processes = crate::process_tree::unix_process_snapshot().await?;
    };

    if !remaining.is_empty() {
        let owned_process_group = process_group_id.filter(|process_group_id| {
            processes.iter().any(|process| {
                process.process_group_id == *process_group_id
                    && remaining.contains(&process.process_id)
            })
        });
        crate::process_tree::signal_process_group(owned_process_group, libc::SIGKILL)?;
        crate::process_tree::signal_processes(&remaining, libc::SIGKILL)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let final_snapshot = crate::process_tree::unix_process_snapshot().await?;
        let live_process_ids =
            crate::process_tree::matching_process_ids(&final_snapshot, &known_processes);
        let stubborn_processes = remaining
            .intersection(&live_process_ids)
            .copied()
            .collect::<Vec<_>>();
        if !stubborn_processes.is_empty() {
            anyhow::bail!("强制停止 Codex 进程超时：{stubborn_processes:?}");
        }
    }
    Ok(known_processes.len())
}

#[cfg(target_os = "macos")]
async fn macos_codex_process_ids(app_dir: &std::path::Path) -> Result<Vec<u32>> {
    let processes = crate::process_tree::unix_process_snapshot().await?;
    Ok(
        owned_unix_codex_process_ids(&processes, app_dir, None, None, None)
            .into_iter()
            .collect(),
    )
}

#[cfg(target_os = "macos")]
async fn macos_codex_is_running(app_dir: &std::path::Path) -> Result<bool> {
    Ok(!macos_codex_process_ids(app_dir).await?.is_empty())
}

#[cfg(windows)]
fn windows_path_is_within(path: &Path, directory: &Path) -> bool {
    let path = normalized_windows_path(path);
    let directory = normalized_windows_path(directory);
    path == directory
        || path
            .strip_prefix(&directory)
            .is_some_and(|rest| rest.starts_with('\\'))
}

#[cfg(windows)]
async fn terminate_windows_codex_processes(app_dir: &Path, process_id: Option<u32>) -> Result<()> {
    let processes = codex_plus_core::windows_enumerate_processes();
    let mut process_ids = processes
        .iter()
        .filter(|process| {
            Some(process.process_id) == process_id
                || process
                    .executable_path
                    .as_deref()
                    .is_some_and(|path| windows_path_is_within(path, app_dir))
        })
        .map(|process| process.process_id)
        .collect::<HashSet<_>>();
    loop {
        let previous_len = process_ids.len();
        for process in &processes {
            if process_ids.contains(&process.parent_process_id) {
                process_ids.insert(process.process_id);
            }
        }
        if process_ids.len() == previous_len {
            break;
        }
    }
    process_ids.remove(&std::process::id());
    for process_id in &process_ids {
        terminate_windows_process(*process_id).await?;
    }
    let remaining = codex_plus_core::windows_enumerate_processes()
        .into_iter()
        .filter(|process| process_ids.contains(&process.process_id))
        .map(|process| process.process_id)
        .collect::<Vec<_>>();
    if !remaining.is_empty() {
        anyhow::bail!("强制停止 Windows Codex 进程超时：{remaining:?}");
    }
    Ok(())
}

#[cfg(windows)]
async fn terminate_windows_process(process_id: u32) -> Result<()> {
    let mut command = Command::new("taskkill");
    command
        .args(["/PID", &process_id.to_string(), "/T", "/F"])
        .creation_flags(codex_plus_core::windows_create_no_window());
    let status = command
        .status()
        .await
        .with_context(|| format!("终止 Windows Codex 进程 {process_id} 失败"))?;
    if !status.success()
        && codex_plus_core::windows_enumerate_processes()
            .iter()
            .any(|process| process.process_id == process_id)
    {
        anyhow::bail!("终止 Windows Codex 进程 {process_id} 失败：taskkill 返回 {status}");
    }
    Ok(())
}

#[cfg(test)]
mod gpu_launch_argument_tests {
    use super::*;

    #[test]
    fn gpu_launch_arguments_are_mutually_exclusive_and_platform_gated() {
        assert!(gpu_launch_arguments(GpuLaunchMode::Off, true).is_empty());
        assert_eq!(
            gpu_launch_arguments(GpuLaunchMode::DisableGpu, true),
            vec![DISABLE_GPU_ARGUMENT.to_string()]
        );
        assert_eq!(
            gpu_launch_arguments(GpuLaunchMode::DisableGpuRasterization, true),
            vec![DISABLE_GPU_RASTERIZATION_ARGUMENT.to_string()]
        );
        assert!(gpu_launch_arguments(GpuLaunchMode::DisableGpu, false).is_empty());
        assert!(gpu_launch_arguments(GpuLaunchMode::DisableGpuRasterization, false).is_empty());
    }
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

    #[test]
    fn owned_codex_tree_includes_bundle_helpers_and_external_descendants() {
        let processes = crate::process_tree::parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT --inspect\n\
              101 100 100 Thu Jul 23 19:23:13 2026 /Applications/ChatGPT.app/Contents/Resources/codex app-server\n\
              102 101 102 Thu Jul 23 19:23:14 2026 node ./mcp/server.mjs\n\
              103 1 103 Thu Jul 23 19:23:15 2026 /Applications/ChatGPT.app/Contents/Frameworks/browser_crashpad_handler\n\
              200 1 200 Thu Jul 23 19:23:16 2026 unrelated\n",
        );
        assert_eq!(
            owned_unix_codex_process_ids(
                &processes,
                Path::new("/Applications/ChatGPT.app"),
                None,
                None,
                Some("--inspect"),
            ),
            HashSet::from([100, 101, 102, 103])
        );
    }

    #[tokio::test]
    async fn unix_shutdown_terminates_the_spawned_process_group() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & wait"]);
        command.process_group(0);
        let mut child = command.spawn().expect("spawn process tree");
        let process_id = child.id().expect("child process id");

        terminate_unix_codex_processes(
            Path::new("/definitely-not-a-real-codex-app"),
            Some(process_id),
            Some(process_id),
            None,
        )
        .await
        .expect("terminate process tree");

        tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .expect("root process was left running")
            .expect("wait for root process");
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
