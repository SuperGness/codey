use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
#[cfg(all(test, unix))]
use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result};
use codey_runtime_core::app_paths::resolve_codex_app_dir_with_saved;
use codey_runtime_core::launcher::{
    ProtocolProxyHandle, build_codex_command, start_protocol_proxy,
};
use codey_runtime_core::settings::{BackendSettings, RelayMode, RelayProfile, RelayProtocol};
use codey_runtime_data::{ProviderSyncResult, ProviderSyncStatus};
use serde::Serialize;
use tokio::process::Child;
#[cfg(not(windows))]
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, oneshot};

use crate::cc_switch::{self, RouteTakeoverState};
use crate::cdp;
use crate::codex_config::{
    RuntimeProviderConfigOptions, apply_runtime_provider_config, codex_home,
    current_model_provider, restore_runtime_cc_switch_provider_config,
    restore_runtime_provider_config,
};
use crate::config::{CodeyConfig, GpuLaunchMode, ProviderProfile};
use crate::crashpad_pending_guard::{self, CrashpadPendingStatsHandle};
use crate::error_log;
use crate::maintenance_lock;
use crate::message_delete;
use crate::model_catalog;
use crate::pet_slim_patch;
use crate::provider_lease;
use crate::session_index_cleanup::{self, SessionIndexCleanupReport};
use crate::startup_maintenance::{self, ProviderSyncPlan};
use crate::subagent_policy;
use crate::trace_log_guard;

mod platform;
mod process;
mod route_overlay;

use platform::*;
use process::{
    SpawnedCodex, prepare_codex_for_launch, reap_child_after_cleanup, spawn_codex,
    spawn_codex_exit_watcher,
};
#[cfg(test)]
use process::{codex_runtime_arguments, gpu_launch_arguments};
use route_overlay::{RouteFilesSnapshot, read_route_files, spawn_route_overlay_watcher};

const CDP_WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
const CDP_WATCHDOG_FAILURE_THRESHOLD: u8 = 2;
pub const CODEX_APP_NOT_FOUND_ERROR: &str = "找不到 Codex 桌面应用";
pub const CODEX_APP_PATH_INVALID_ERROR: &str = "配置的 Codex App 路径无效或指向了 Codex CLI；请选择 Codex 桌面 App，不要选择 codex.exe 命令行程序";
const DISABLE_GPU_ARGUMENT: &str = "--disable-gpu";
const DISABLE_GPU_RASTERIZATION_ARGUMENT: &str = "--disable-gpu-rasterization";
const DEFAULT_CHINESE_LOCALE_ARGUMENT: &str = "--lang=zh-CN";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceStatus {
    pub session_status: String,
    pub session_files_fixed: usize,
    pub sqlite_rows_updated: usize,
    pub ghost_tasks_pruned: usize,
    pub performance_status: String,
    pub performance_detail: String,
}

struct SessionMaintenanceSummary {
    status: String,
    files_fixed: usize,
    sqlite_rows_updated: usize,
    ghost_tasks_pruned: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeModelConfig {
    selected_models: Vec<String>,
    upstream_models: Vec<String>,
    default_model: Option<String>,
}

impl RuntimeModelConfig {
    pub fn from_config(config: &CodeyConfig) -> Self {
        Self {
            selected_models: config.selected_models().to_vec(),
            upstream_models: config.upstream_models().to_vec(),
            default_model: config.default_model().map(ToString::to_string),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSubagentConfig {
    model: String,
    reasoning_effort: String,
    roles: std::collections::BTreeMap<String, crate::config::SubagentRoleConfig>,
}

impl RuntimeSubagentConfig {
    pub fn from_config(config: &CodeyConfig) -> Self {
        Self {
            model: config.subagent_model.clone(),
            reasoning_effort: config.subagent_reasoning_effort.clone(),
            roles: config.subagent_roles.clone(),
        }
    }
}

pub struct CodeyRuntime {
    pub codex_app_path: PathBuf,
    pub maintenance: MaintenanceStatus,
    pub applied_config: CodeyConfig,
    applied_model_config: RwLock<RuntimeModelConfig>,
    applied_subagent_config: RwLock<RuntimeSubagentConfig>,
    pub injection_statuses: Arc<RwLock<Arc<[cdp::InjectionScriptStatus]>>>,
    injection_scripts: cdp::PreparedInjectionScripts,
    injection_websocket_url: Arc<RwLock<Arc<str>>>,
    child: Arc<Mutex<Option<Child>>>,
    process_id: Option<u32>,
    #[cfg(unix)]
    process_group_id: Option<u32>,
    #[cfg(target_os = "macos")]
    inspector_argument: Option<String>,
    watchdog_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    watchdog_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    route_overlay_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    route_overlay_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    exit_watchdog_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    exit_watchdog_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    crashpad_guard_enabled: Arc<AtomicBool>,
    crashpad_guard_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    crashpad_guard_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    protocol_proxy: Mutex<Option<ProtocolProxyHandle>>,
}

fn protocol_proxy_settings(
    profile: &ProviderProfile,
    default_model: Option<&str>,
    third_party_models: &[String],
) -> Option<BackendSettings> {
    if profile.cc_switch_read_only {
        return None;
    }
    let chat_completions_models = match profile.protocol {
        RelayProtocol::ChatCompletions => Vec::new(),
        RelayProtocol::Responses => {
            // Responses 线路只有在存在第三方模型（claude/kimi 等，通常不支持
            // /v1/responses）时才经本地代理；官方模型逐请求直通，行为不变。
            if third_party_models.is_empty() {
                return None;
            }
            third_party_models.to_vec()
        }
    };
    let base_url = profile.normalized_base_url();
    let relay = RelayProfile {
        id: profile.id.clone(),
        name: profile.name.clone(),
        model: default_model.unwrap_or_default().to_string(),
        base_url: base_url.clone(),
        upstream_base_url: base_url,
        api_key: profile.api_key.clone(),
        protocol: profile.protocol,
        relay_mode: RelayMode::PureApi,
        chat_completions_models,
        ..RelayProfile::default()
    };
    Some(BackendSettings {
        active_relay_id: relay.id.clone(),
        relay_profiles: vec![relay],
        enhancements_enabled: false,
        ..BackendSettings::default()
    })
}

async fn start_runtime_protocol_proxy(
    profile: &ProviderProfile,
    default_model: Option<&str>,
    third_party_models: &[String],
) -> Result<Option<ProtocolProxyHandle>> {
    let Some(settings) = protocol_proxy_settings(profile, default_model, third_party_models) else {
        return Ok(None);
    };
    start_protocol_proxy(settings)
        .await
        .map(Some)
        .context("启动本地协议代理失败")
}

async fn resolve_startup_provider(home: &std::path::Path) -> Result<String> {
    let provider_home = home.to_path_buf();
    tokio::task::spawn_blocking(move || current_model_provider(&provider_home))
        .await
        .map_err(|error| {
            let error = anyhow::Error::new(error).context("读取当前模型 Provider 任务异常退出");
            error_log::record_failure(
                "patch_failed",
                "read_current_model_provider",
                format!("{error:#}"),
                serde_json::json!({
                    "codexHome": home,
                    "taskJoinFailed": true,
                }),
            );
            error
        })?
        .map_err(|error| {
            error_log::record_failure(
                "patch_failed",
                "read_current_model_provider",
                format!("{error:#}"),
                serde_json::json!({
                    "codexHome": home,
                }),
            );
            error
        })
}

async fn run_startup_session_maintenance(
    home: &std::path::Path,
    provider: &str,
) -> Result<SessionMaintenanceSummary> {
    let maintenance_home = home.to_path_buf();
    let maintenance_provider = provider.to_string();
    let maintenance_result = tokio::task::spawn_blocking(move || {
        let stale_lock_recovery = maintenance_lock::recover_stale_locks(&maintenance_home);
        let provider_sync =
            match startup_maintenance::provider_sync_plan(&maintenance_home, &maintenance_provider)
            {
                Ok(ProviderSyncPlan::Cached) => {
                    startup_maintenance::cached_provider_sync_result(&maintenance_provider)
                }
                Ok(ProviderSyncPlan::Full) | Err(_) => {
                    let result = codey_runtime_data::run_provider_sync_with_target(
                        Some(&maintenance_home),
                        Some(&maintenance_provider),
                    );
                    if result.status == ProviderSyncStatus::Synced
                        && result.skipped_locked_rollout_files.is_empty()
                        && let Err(error) = startup_maintenance::record_provider_sync_success(
                            &maintenance_home,
                            &maintenance_provider,
                        )
                    {
                        error_log::record_failure(
                            "patch_failed",
                            "record_provider_sync_success",
                            format!("{error:#}"),
                            serde_json::json!({
                                "provider": maintenance_provider,
                            }),
                        );
                        eprintln!("保存 Provider 同步状态失败：{error:#}");
                    }
                    result
                }
            };
        // A loaded Codex thread may have flushed a deleted turn after the live
        // request completed. Reapply durable tombstones after the old process
        // is stopped and before the new process can hydrate that stale data.
        let message_delete_replay = message_delete::reapply_persisted_deletions(&maintenance_home);
        // `session_index.jsonl` is also cleaned before spawn, while its
        // source snapshot is stable. The original file is backed up.
        let index_cleanup = session_index_cleanup::cleanup(&maintenance_home);
        (
            stale_lock_recovery,
            provider_sync,
            message_delete_replay,
            index_cleanup,
        )
    })
    .await;
    let (stale_lock_recovery, provider_sync, message_delete_replay, index_cleanup) =
        match maintenance_result {
            Ok(result) => result,
            Err(error) => {
                let error = anyhow::Error::new(error).context("启动前会话修复任务异常退出");
                error_log::record_failure(
                    "patch_failed",
                    "run_startup_session_repairs",
                    format!("{error:#}"),
                    serde_json::json!({
                        "codexHome": home,
                    }),
                );
                return Err(error);
            }
        };
    match stale_lock_recovery {
        Ok(recovered) => {
            for path in recovered {
                eprintln!("已清理陈旧维护锁：{}", path.display());
            }
        }
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "recover_stale_maintenance_locks",
                format!("{error:#}"),
                serde_json::json!({
                    "codexHome": home,
                }),
            );
            eprintln!("清理陈旧维护锁失败：{error:#}");
        }
    }
    if provider_sync.status != ProviderSyncStatus::Synced {
        error_log::record_failure(
            "patch_failed",
            "sync_session_providers",
            provider_sync.message.clone(),
            serde_json::json!({
                "status": format!("{:?}", provider_sync.status),
                "targetProvider": provider_sync.target_provider,
                "skippedLockedFiles": provider_sync.skipped_locked_rollout_files.len(),
            }),
        );
    } else if !provider_sync.skipped_locked_rollout_files.is_empty() {
        error_log::record_failure(
            "patch_failed",
            "sync_session_providers",
            format!(
                "跳过 {} 个被占用的会话文件",
                provider_sync.skipped_locked_rollout_files.len()
            ),
            serde_json::json!({
                "targetProvider": provider_sync.target_provider,
                "skippedLockedFiles": provider_sync.skipped_locked_rollout_files,
            }),
        );
    }
    match message_delete_replay {
        Ok(summary) => {
            if summary.deleted > 0 {
                eprintln!(
                    "启动前重新清理了 {} 个已删除对话轮（{} 个会话）",
                    summary.deleted, summary.cleared_sessions
                );
            }
            for (session_id, message) in summary.failures {
                error_log::record_failure(
                    "patch_failed",
                    "reapply_message_deletion",
                    message,
                    serde_json::json!({
                        "sessionId": session_id,
                    }),
                );
            }
        }
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "reapply_message_deletions",
                format!("{error:#}"),
                serde_json::json!({
                    "codexHome": home,
                }),
            );
            eprintln!("启动前重施消息删除失败：{error:#}");
        }
    }
    if let Err(error) = &index_cleanup {
        error_log::record_failure(
            "patch_failed",
            "cleanup_session_index",
            format!("{error:#}"),
            serde_json::json!({
                "codexHome": home,
            }),
        );
    }
    Ok(session_maintenance_summary(&provider_sync, &index_cleanup))
}

async fn resolve_configured_codex_app_dir(config: &CodeyConfig) -> Result<PathBuf> {
    let configured_app_path = config.codex_app_path.trim();
    let configured_app_path_is_empty = configured_app_path.is_empty();
    let configured_app_path =
        (!configured_app_path_is_empty).then(|| PathBuf::from(configured_app_path));
    tokio::task::spawn_blocking(move || {
        resolve_codex_app_dir_with_saved(configured_app_path.as_deref(), None)
    })
    .await
    .map_err(|error| anyhow::Error::new(error).context("定位 Codex App 任务异常退出"))?
    .ok_or_else(|| {
        if configured_app_path_is_empty {
            anyhow::anyhow!(CODEX_APP_NOT_FOUND_ERROR)
        } else {
            anyhow::anyhow!(CODEX_APP_PATH_INVALID_ERROR)
        }
    })
}

struct CodexStartupStateOptions<'a> {
    original_provider: &'a str,
    preserve_provider_route: bool,
    protocol_proxy_base_url: Option<&'a str>,
    expected_config: Option<&'a [u8]>,
}

struct StartupModelCatalog {
    use_official_catalog: bool,
    model_state: model_catalog::ModelSelectionState,
}

struct PreparedCodexStartupState {
    config_contents: Vec<u8>,
    runtime_config: CodeyConfig,
    runtime_config_overrides: Vec<String>,
}

async fn prepare_startup_model_catalog(
    config: &CodeyConfig,
    current_profile: &ProviderProfile,
    home: &std::path::Path,
    preserve_provider_route: bool,
) -> Result<StartupModelCatalog> {
    if preserve_provider_route {
        return Ok(StartupModelCatalog {
            use_official_catalog: false,
            model_state: model_catalog::ModelSelectionState::default(),
        });
    }

    let catalog_home = home.to_path_buf();
    let official_provider = current_profile.cc_switch_read_only;
    let upstream_models = config.upstream_models_snapshot().map(<[String]>::to_vec);
    let selected_models = config.selected_models().to_vec();
    let manual_models = config.manual_third_party_models().to_vec();
    let requested_default_model = config.default_model().map(str::to_owned);
    let (refresh_result, catalog_available, selection_result) =
        tokio::task::spawn_blocking(move || {
            let refresh = model_catalog::refresh_for_provider(
                &catalog_home,
                official_provider,
                upstream_models.as_deref(),
                &selected_models,
            );
            let catalog_available = refresh.is_err() && model_catalog::is_available(&catalog_home);
            let selection = model_catalog::selection_state_with_manual_models(
                &catalog_home,
                official_provider,
                upstream_models.as_deref(),
                &selected_models,
                &manual_models,
                requested_default_model.as_deref(),
            );
            (refresh, catalog_available, selection)
        })
        .await
        .map_err(|error| {
            let error = anyhow::Error::new(error).context("准备模型目录任务异常退出");
            error_log::record_failure(
                "patch_failed",
                "prepare_model_catalog",
                format!("{error:#}"),
                serde_json::json!({
                    "officialProvider": official_provider,
                    "taskJoinFailed": true,
                }),
            );
            error
        })?;

    let use_official_catalog = match refresh_result {
        Ok(_) => true,
        Err(error) if model_catalog::is_runtime_model_cache_unavailable(&error) => {
            if catalog_available {
                eprintln!("本机官方模型缓存暂不含自定义目录必需字段，沿用上一份合法镜像");
            } else {
                eprintln!("本机官方模型缓存暂不含自定义目录必需字段，使用 Codex 内置模型目录");
            }
            catalog_available
        }
        Err(error) if catalog_available => {
            error_log::record_failure(
                "patch_failed",
                "refresh_model_catalog",
                format!("{error:#}"),
                serde_json::json!({
                    "fallback": "last_valid_catalog",
                    "officialProvider": official_provider,
                }),
            );
            eprintln!("刷新官方账号模型目录失败，沿用上一份合法镜像：{error:#}");
            true
        }
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "refresh_model_catalog",
                format!("{error:#}"),
                serde_json::json!({
                    "fallback": "codex_builtin_catalog",
                    "officialProvider": official_provider,
                }),
            );
            eprintln!("刷新官方账号模型目录失败，临时使用 Codex 内置目录：{error:#}");
            false
        }
    };
    let model_state = match selection_result {
        Ok(state) => state,
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "read_model_catalog_selection",
                format!("{error:#}"),
                serde_json::json!({
                    "fallback": "empty_default_model",
                    "officialProvider": official_provider,
                }),
            );
            model_catalog::ModelSelectionState::default()
        }
    };
    Ok(StartupModelCatalog {
        use_official_catalog,
        model_state,
    })
}

async fn prepare_codex_startup_state(
    config: &CodeyConfig,
    current_profile: &ProviderProfile,
    home: &std::path::Path,
    options: CodexStartupStateOptions<'_>,
    startup_catalog: StartupModelCatalog,
) -> Result<PreparedCodexStartupState> {
    let CodexStartupStateOptions {
        original_provider,
        preserve_provider_route,
        protocol_proxy_base_url,
        expected_config,
    } = options;
    let StartupModelCatalog {
        use_official_catalog,
        model_state,
    } = startup_catalog;
    let runtime_config_home = home.to_path_buf();
    let runtime_config_profile = current_profile.clone();
    let runtime_config_provider = original_provider.to_string();
    let runtime_default_model =
        (!model_state.default_model.is_empty()).then_some(model_state.default_model.clone());
    let fast_context_tools = config.fast_context_tools;
    let mut runtime_subagent_config = config.clone();
    subagent_policy::reconcile_with_model_state(&mut runtime_subagent_config, Some(&model_state));
    let subagent_optimization = runtime_subagent_config.subagent_optimization;
    let subagent_model = runtime_subagent_config.subagent_model.clone();
    let subagent_reasoning_effort = runtime_subagent_config.subagent_reasoning_effort.clone();
    let subagent_roles = runtime_subagent_config.subagent_roles.clone();
    let protocol_proxy_base_url = protocol_proxy_base_url.map(str::to_string);
    let protocol_proxy_enabled = protocol_proxy_base_url.is_some();
    let expected_config = expected_config.map(<[u8]>::to_vec);
    let runtime_config = tokio::task::spawn_blocking(move || {
        apply_runtime_provider_config(
            &runtime_config_home,
            &runtime_config_profile,
            &runtime_config_provider,
            RuntimeProviderConfigOptions {
                use_official_catalog,
                default_model: runtime_default_model.as_deref(),
                fast_context_tools,
                subagent_optimization,
                subagent_model: &subagent_model,
                subagent_reasoning_effort: &subagent_reasoning_effort,
                subagent_roles: Some(&subagent_roles),
                preserve_provider_route,
                protocol_proxy_base_url: protocol_proxy_base_url.as_deref(),
                expected_config: expected_config.as_deref(),
            },
        )
    })
    .await
    .map_err(|error| {
        let error = anyhow::Error::new(error).context("应用运行时 Provider 配置任务异常退出");
        error_log::record_failure(
            "patch_failed",
            "apply_runtime_provider_config",
            format!("{error:#}"),
            serde_json::json!({
                "profile": current_profile.name,
                "provider": original_provider,
                "fastContextTools": config.fast_context_tools,
                "subagentOptimization": config.subagent_optimization,
                "preserveProviderRoute": preserve_provider_route,
                "protocolProxyEnabled": protocol_proxy_enabled,
                "taskJoinFailed": true,
            }),
        );
        error
    })?;
    let applied = runtime_config.map_err(|error| {
        error_log::record_failure(
            "patch_failed",
            "apply_runtime_provider_config",
            format!("{error:#}"),
            serde_json::json!({
                "profile": current_profile.name,
                "provider": original_provider,
                "fastContextTools": config.fast_context_tools,
                "subagentOptimization": config.subagent_optimization,
                "preserveProviderRoute": preserve_provider_route,
                "protocolProxyEnabled": protocol_proxy_enabled,
            }),
        );
        error
    })?;
    Ok(PreparedCodexStartupState {
        config_contents: applied.config_contents,
        runtime_config: runtime_subagent_config,
        runtime_config_overrides: applied.runtime_config_overrides,
    })
}

async fn await_initial_storage_guards(
    initial_trace_guard: tokio::task::JoinHandle<Result<trace_log_guard::TraceLogGuardReport>>,
    disable_trace_log_writes: bool,
    initial_crashpad_guard: tokio::task::JoinHandle<crashpad_pending_guard::CrashpadGuardRun>,
    protect_crashpad_pending: bool,
    crashpad_pending_stats: &CrashpadPendingStatsHandle,
) -> Result<()> {
    match initial_trace_guard.await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            error_log::record_failure(
                "patch_failed",
                "configure_trace_log_guard",
                format!("{error:#}"),
                serde_json::json!({
                    "disabled": disable_trace_log_writes,
                }),
            );
            return Err(error);
        }
        Err(error) => {
            let error = anyhow::Error::new(error).context("Trace 日志保护切换任务异常退出");
            error_log::record_failure(
                "patch_failed",
                "configure_trace_log_guard",
                format!("{error:#}"),
                serde_json::json!({
                    "disabled": disable_trace_log_writes,
                }),
            );
            return Err(error);
        }
    }

    match initial_crashpad_guard.await {
        Ok(run) => {
            if !run.cleanup.errors.is_empty() || run.cleanup.still_over_limit {
                error_log::record_failure(
                    "cleanup_failed",
                    "enforce_crashpad_pending_limit_at_startup",
                    if run.cleanup.still_over_limit {
                        "Crashpad pending 仍超过安全上限".to_string()
                    } else {
                        format!(
                            "{} 个 Crashpad 待处理文件未能完成收敛",
                            run.cleanup.errors.len()
                        )
                    },
                    serde_json::json!({
                        "errorCount": run.cleanup.errors.len(),
                        "stillOverLimit": run.cleanup.still_over_limit,
                        "bytesReclaimed": run.cleanup.bytes_reclaimed,
                    }),
                );
            }
            crashpad_pending_stats.replace(run.snapshot);
        }
        Err(error) => {
            let error = format!("Crashpad 磁盘保护任务异常退出：{error}");
            error_log::record_failure(
                "cleanup_failed",
                "enforce_crashpad_pending_limit_at_startup",
                error.clone(),
                serde_json::json!({
                    "taskJoinFailed": true,
                }),
            );
            let mut snapshot = crashpad_pending_guard::CrashpadPendingStatsSnapshot::idle(
                protect_crashpad_pending,
            );
            snapshot.errors.push(error);
            crashpad_pending_stats.replace(snapshot);
        }
    }
    Ok(())
}

type PetSlimTaskResult =
    std::result::Result<Result<pet_slim_patch::PetSlimReport>, tokio::task::JoinError>;

async fn configure_startup_pet(home: &std::path::Path, slim_codex_pet: bool) -> PetSlimTaskResult {
    let pet_home = home.to_path_buf();
    tokio::task::spawn_blocking(move || pet_slim_patch::configure(&pet_home, slim_codex_pet)).await
}

async fn stop_runtime_watcher(
    shutdown: &Mutex<Option<oneshot::Sender<()>>>,
    task: &Mutex<Option<tokio::task::JoinHandle<()>>>,
    failure_event: &'static str,
    failure_operation: &'static str,
    failure_message: &'static str,
) {
    if let Some(sender) = shutdown.lock().await.take() {
        let _ = sender.send(());
    }
    let task = task.lock().await.take();
    if let Some(task) = task
        && let Err(error) = task.await
    {
        error_log::record_failure(
            failure_event,
            failure_operation,
            error.to_string(),
            serde_json::json!({}),
        );
        eprintln!("{failure_message}：{error}");
    }
}

async fn stop_codex_processes(
    app_dir: &std::path::Path,
    process_id: Option<u32>,
    #[cfg(unix)] process_group_id: Option<u32>,
    #[cfg(target_os = "macos")] inspector_argument: Option<&str>,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        if let Some(inspector_argument) = inspector_argument {
            return stop_macos_codex(inspector_argument, app_dir, process_id, process_group_id)
                .await;
        }
        terminate_unix_codex_processes(app_dir, process_id, process_group_id, None)
            .await
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        terminate_unix_codex_processes(app_dir, process_id, process_group_id, None)
            .await
            .map(|_| ())
    }
    #[cfg(windows)]
    {
        terminate_windows_codex_processes(app_dir, process_id).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (app_dir, process_id);
        Ok(())
    }
}

fn injection_failure_cleanup_operation() -> &'static str {
    #[cfg(windows)]
    {
        "cleanup_windows_after_injection_failure"
    }
    #[cfg(target_os = "macos")]
    {
        "cleanup_macos_after_injection_failure"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "cleanup_unix_after_injection_failure"
    }
    #[cfg(not(any(unix, windows)))]
    {
        "cleanup_after_injection_failure"
    }
}

async fn inject_initial_renderer(
    debug_port: u16,
    handler: codey_runtime_core::bridge::BridgeHandler,
    injection_scripts: &cdp::PreparedInjectionScripts,
    app_dir: &std::path::Path,
    home: &std::path::Path,
    spawned: &SpawnedCodex,
    child: &Arc<Mutex<Option<Child>>>,
) -> Result<cdp::InjectedTarget> {
    let failure = match cdp::retry_inject_with_scripts(debug_port, handler, injection_scripts).await
    {
        Ok(target) => return Ok(target),
        Err(failure) => failure,
    };
    let error_message = format!("{failure:#}");
    let failure_metadata = error_log::FailureMetadata {
        stage: Some("startup.renderer_injection".to_string()),
        recoverable: Some(false),
    };
    let error = failure.into_error();
    error_log::record_failure_with_metadata(
        "injection_failed",
        "inject_cdp_bridge",
        error_message,
        failure_metadata,
        serde_json::json!({
            "appPath": app_dir,
            "debugPort": debug_port,
            "processId": spawned.process_id,
        }),
    );

    if let Err(stop_error) = stop_codex_processes(
        app_dir,
        spawned.process_id,
        #[cfg(unix)]
        spawned.process_group_id,
        #[cfg(target_os = "macos")]
        spawned.inspector_argument.as_deref(),
    )
    .await
    {
        let context = serde_json::json!({
            "appPath": app_dir,
            "processId": spawned.process_id,
        });
        #[cfg(unix)]
        let context = {
            let mut context = context;
            if let Some(context) = context.as_object_mut() {
                context.insert(
                    "processGroupId".to_string(),
                    serde_json::json!(spawned.process_group_id),
                );
            }
            context
        };
        error_log::record_failure(
            "cleanup_failed",
            injection_failure_cleanup_operation(),
            format!("{stop_error:#}"),
            context,
        );
        eprintln!("Codex 注入失败后的进程清理失败：{stop_error:#}");
    }
    if let Some(child) = child.lock().await.take() {
        reap_child_after_cleanup(child, "reap_child_after_injection_failure").await;
    }
    Err(restore_runtime_config_after_error(home, error).await)
}

struct InjectionWatchdog {
    statuses: Arc<RwLock<Arc<[cdp::InjectionScriptStatus]>>>,
    websocket_url: Arc<RwLock<Arc<str>>>,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

fn spawn_injection_watchdog(
    injected_target: cdp::InjectedTarget,
    debug_port: u16,
    handler: codey_runtime_core::bridge::BridgeHandler,
    injection_scripts: cdp::PreparedInjectionScripts,
) -> InjectionWatchdog {
    let statuses = Arc::new(RwLock::new(injected_target.injection_statuses()));
    let websocket_url = Arc::new(RwLock::new(injected_target.websocket_url_arc()));
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let watchdog_statuses = statuses.clone();
    let watchdog_websocket_url = websocket_url.clone();
    let watchdog_scripts = injection_scripts;
    let task = tokio::spawn(async move {
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
                    match result {
                        Ok(healthy) => healthy,
                        Err(error) => {
                            error_log::record_failure_async(
                                "injection_health_check_failed",
                                "check_cdp_bridge_health",
                                format!("{error:#}"),
                                serde_json::json!({
                                    "websocketUrl": target.websocket_url(),
                                }),
                            )
                            .await;
                            false
                        }
                    }
                }
            };
            if !watchdog_should_reinject(&mut consecutive_failures, healthy) {
                continue;
            }
            let reinjection = tokio::select! {
                biased;
                _ = &mut shutdown_rx => break 'watchdog,
                result = cdp::retry_inject_with_scripts(
                    debug_port,
                    handler.clone(),
                    &watchdog_scripts,
                ) => result,
            };
            match reinjection {
                Ok(reinjected) => {
                    let next_statuses = reinjected.injection_statuses();
                    let next_websocket_url = reinjected.websocket_url_arc();
                    let previous = std::mem::replace(&mut target, reinjected);
                    *watchdog_statuses.write().await = next_statuses;
                    *watchdog_websocket_url.write().await = next_websocket_url;
                    previous.close().await;
                    consecutive_failures = 0;
                }
                Err(error) => {
                    let error_message = format!("{error:#}");
                    error_log::record_failure_with_metadata_async(
                        "injection_failed",
                        "reinject_cdp_bridge",
                        error_message.clone(),
                        error_log::FailureMetadata {
                            stage: Some("runtime.renderer_reinjection".to_string()),
                            recoverable: Some(true),
                        },
                        serde_json::json!({
                            "debugPort": debug_port,
                        }),
                    )
                    .await;
                    *watchdog_statuses.write().await = watchdog_scripts
                        .statuses_with_error(format!("脚本重新注入失败：{error_message}"));
                    eprintln!("Codey CDP bridge 恢复失败：{error_message}");
                    consecutive_failures = CDP_WATCHDOG_FAILURE_THRESHOLD.saturating_sub(1);
                }
            }
        }
        target.close().await;
    });
    InjectionWatchdog {
        statuses,
        websocket_url,
        shutdown,
        task,
    }
}

struct InitialStorageGuards {
    trace: tokio::task::JoinHandle<Result<trace_log_guard::TraceLogGuardReport>>,
    crashpad: tokio::task::JoinHandle<crashpad_pending_guard::CrashpadGuardRun>,
}

struct StartupRouteContext {
    preserve_provider_route: bool,
    live_route: Option<cc_switch::LiveRouteSnapshot>,
    original_provider: String,
    current_profile: ProviderProfile,
}

struct StartupStorageState {
    app_dir: PathBuf,
    session_maintenance: SessionMaintenanceSummary,
}

struct PreparedProviderState {
    protocol_proxy: Option<ProtocolProxyHandle>,
    applied_route_files: Option<RouteFilesSnapshot>,
    runtime_config: CodeyConfig,
    runtime_config_overrides: Vec<String>,
}

struct StartupPatchState {
    debug_port: u16,
    route_overlay_shutdown: Option<oneshot::Sender<()>>,
    route_overlay_task: Option<tokio::task::JoinHandle<()>>,
    route_changed: Option<oneshot::Receiver<()>>,
}

struct SpawnedRenderer {
    app_dir: PathBuf,
    spawned: SpawnedCodex,
    child: Arc<Mutex<Option<Child>>>,
    maintenance: MaintenanceStatus,
    injected_target: cdp::InjectedTarget,
}

struct RuntimeWatchers {
    injection_statuses: Arc<RwLock<Arc<[cdp::InjectionScriptStatus]>>>,
    injection_websocket_url: Arc<RwLock<Arc<str>>>,
    watchdog_shutdown: oneshot::Sender<()>,
    watchdog_task: tokio::task::JoinHandle<()>,
    crashpad_guard_enabled: Arc<AtomicBool>,
    crashpad_guard_shutdown: oneshot::Sender<()>,
    crashpad_guard_task: tokio::task::JoinHandle<()>,
    exit_watchdog_shutdown: oneshot::Sender<()>,
    exit_watchdog_task: tokio::task::JoinHandle<()>,
    codex_exit: oneshot::Receiver<()>,
}

struct RuntimeWatcherInputs {
    injected_target: cdp::InjectedTarget,
    debug_port: u16,
    handler: codey_runtime_core::bridge::BridgeHandler,
    injection_scripts: cdp::PreparedInjectionScripts,
    child: Arc<Mutex<Option<Child>>>,
    process_id: Option<u32>,
    protect_crashpad_pending: bool,
    crashpad_pending_stats: CrashpadPendingStatsHandle,
}

fn spawn_initial_storage_guards(
    home: &std::path::Path,
    config: &CodeyConfig,
) -> InitialStorageGuards {
    let trace_guard_home = home.to_path_buf();
    let disable_trace_log_writes = config.disable_trace_log_writes;
    let trace = tokio::task::spawn_blocking(move || {
        trace_log_guard::configure(&trace_guard_home, disable_trace_log_writes)
    });
    let protect_crashpad_pending = config.protect_crashpad_pending;
    let crashpad = tokio::task::spawn_blocking(move || {
        if protect_crashpad_pending {
            crashpad_pending_guard::enforce_system_limit()
        } else {
            crashpad_pending_guard::CrashpadGuardRun {
                cleanup: crashpad_pending_guard::CrashpadCleanupReport::default(),
                snapshot: crashpad_pending_guard::snapshot_system(false),
            }
        }
    });
    InitialStorageGuards { trace, crashpad }
}

async fn resolve_startup_route_context(
    home: &std::path::Path,
    config: &CodeyConfig,
) -> Result<StartupRouteContext> {
    let startup_route_home = home.to_path_buf();
    let startup_route =
        tokio::task::spawn_blocking(move || cc_switch::startup_route_state(&startup_route_home))
            .await
            .map_err(|error| {
                let error =
                    anyhow::Error::new(error).context("检测 CC Switch 路由接管任务异常退出");
                error_log::record_failure(
                    "patch_failed",
                    "detect_cc_switch_route_takeover",
                    format!("{error:#}"),
                    serde_json::json!({
                        "codexHome": home,
                        "taskJoinFailed": true,
                    }),
                );
                error
            })?
            .map_err(|error| {
                error_log::record_failure(
                    "patch_failed",
                    "detect_cc_switch_route_takeover",
                    format!("{error:#}"),
                    serde_json::json!({
                        "codexHome": home,
                    }),
                );
                error
            })?;
    let preserve_provider_route =
        preserve_cc_switch_route(startup_route.takeover).map_err(|error| {
            error_log::record_failure(
                "patch_failed",
                "validate_cc_switch_route_takeover",
                format!("{error:#}"),
                serde_json::json!({
                    "managed": startup_route.takeover.managed,
                    "live": startup_route.takeover.live,
                }),
            );
            error
        })?;
    let live_route = if preserve_provider_route {
        Some(
            startup_route
                .live_route
                .ok_or_else(|| anyhow::anyhow!("CC Switch Live 路由缺少已验证的 Provider 快照"))?,
        )
    } else {
        None
    };
    let original_provider = if let Some(live_route) = live_route.as_ref() {
        live_route.provider_id().to_string()
    } else {
        resolve_startup_provider(home).await?
    };
    let current_profile = if let Some(live_route) = live_route.as_ref() {
        live_route.profile().clone()
    } else {
        config
            .active_profile()
            .ok_or_else(|| anyhow::anyhow!("找不到当前 Codex 线路"))?
    };
    Ok(StartupRouteContext {
        preserve_provider_route,
        live_route,
        original_provider,
        current_profile,
    })
}

async fn prepare_startup_storage(
    home: &std::path::Path,
    config: &CodeyConfig,
    original_provider: &str,
    guards: InitialStorageGuards,
    crashpad_pending_stats: &CrashpadPendingStatsHandle,
) -> Result<StartupStorageState> {
    let app_dir = resolve_configured_codex_app_dir(config).await?;
    // Session repair must never race a live Codex writer. Stopping the old
    // runtime first also gives SQLite and rollout buffers a chance to flush
    // before any permanent maintenance is applied.
    prepare_codex_for_launch(&app_dir).await?;

    // Permanent maintenance runs before Codey creates the temporary
    // direct-provider lease. A lightweight header/SQLite validation normally
    // reuses the last successful provider sync; provider changes still
    // fall back to the complete rollout and SQLite repair.
    let session_maintenance = run_startup_session_maintenance(home, original_provider).await?;
    await_initial_storage_guards(
        guards.trace,
        config.disable_trace_log_writes,
        guards.crashpad,
        config.protect_crashpad_pending,
        crashpad_pending_stats,
    )
    .await?;
    Ok(StartupStorageState {
        app_dir,
        session_maintenance,
    })
}

async fn prepare_runtime_provider_state(
    home: &std::path::Path,
    config: &CodeyConfig,
    route: &StartupRouteContext,
) -> Result<PreparedProviderState> {
    // 模型目录先于协议代理准备：Responses 线路是否需要本地代理、以及哪些
    // 模型要走 Chat Completions 转换，都取决于目录里的第三方模型集合。
    let startup_catalog = prepare_startup_model_catalog(
        config,
        &route.current_profile,
        home,
        route.preserve_provider_route,
    )
    .await?;
    let protocol_proxy = start_runtime_protocol_proxy(
        &route.current_profile,
        config.default_model(),
        &startup_catalog.model_state.third_party_models,
    )
    .await
    .map_err(|error| {
        error_log::record_failure(
            "protocol_proxy_start_failed",
            "start_protocol_proxy",
            format!("{error:#}"),
            serde_json::json!({
                "provider": route.original_provider,
                "protocol": route.current_profile.protocol,
                "thirdPartyModels": startup_catalog.model_state.third_party_models.len(),
            }),
        );
        error
    })?;
    let protocol_proxy_base_url = protocol_proxy
        .as_ref()
        .map(|proxy| proxy.base_url().to_string());
    let prepared_startup = prepare_codex_startup_state(
        config,
        &route.current_profile,
        home,
        CodexStartupStateOptions {
            original_provider: &route.original_provider,
            preserve_provider_route: route.preserve_provider_route,
            protocol_proxy_base_url: protocol_proxy_base_url.as_deref(),
            expected_config: route
                .live_route
                .as_ref()
                .map(|route| route.config_contents()),
        },
        startup_catalog,
    )
    .await?;
    let applied_route_files = route
        .live_route
        .as_ref()
        .map(|live_route| RouteFilesSnapshot {
            config: prepared_startup.config_contents,
            auth: live_route.auth_contents().map(<[u8]>::to_vec),
        });
    Ok(PreparedProviderState {
        protocol_proxy,
        applied_route_files,
        runtime_config: prepared_startup.runtime_config,
        runtime_config_overrides: prepared_startup.runtime_config_overrides,
    })
}

async fn prepare_startup_patches_and_overlay(
    home: &std::path::Path,
    config: &CodeyConfig,
    applied_route_files: Option<&RouteFilesSnapshot>,
) -> Result<StartupPatchState> {
    let (route_overlay_shutdown, route_overlay_task, route_changed) =
        if let Some(applied_route_files) = applied_route_files {
            let (shutdown, task, changed) =
                spawn_route_overlay_watcher(home.to_path_buf(), applied_route_files.clone());
            (Some(shutdown), Some(task), Some(changed))
        } else {
            (None, None, None)
        };

    let slim_codex_pet = config.slim_codex_pet;
    let pet_result = configure_startup_pet(home, slim_codex_pet).await;
    let debug_port = codey_runtime_core::ports::select_packaged_codex_debug_port(9229);
    if let Some(applied_route_files) = applied_route_files {
        let current_route_files = read_route_files(home)
            .await
            .with_context(|| "启动 Codex 前复核 CC Switch Live 路由失败")?;
        if current_route_files.as_ref() != Some(applied_route_files) {
            return Err(restore_runtime_config_after_error(
                home,
                anyhow::anyhow!("CC Switch Live 路由在 Codex 启动前发生变化；已取消旧线路启动"),
            )
            .await);
        }
    }
    match pet_result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            error_log::record_failure(
                "patch_failed",
                "configure_codex_pet_slim",
                format!("{error:#}"),
                serde_json::json!({
                    "enabled": slim_codex_pet,
                }),
            );
            return Err(restore_runtime_config_after_error(
                home,
                error.context("应用 Codex 宠物精简设置失败"),
            )
            .await);
        }
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "configure_codex_pet_slim",
                error.to_string(),
                serde_json::json!({
                    "enabled": slim_codex_pet,
                    "taskJoinFailed": true,
                }),
            );
            return Err(restore_runtime_config_after_error(
                home,
                anyhow::Error::new(error).context("Codex 宠物精简设置任务异常退出"),
            )
            .await);
        }
    };
    Ok(StartupPatchState {
        debug_port,
        route_overlay_shutdown,
        route_overlay_task,
        route_changed,
    })
}

// This boundary receives already-resolved launch components from the lifecycle
// coordinator. Explicit parameters keep ownership and cleanup responsibilities visible.
#[allow(clippy::too_many_arguments)]
async fn spawn_and_inject_runtime(
    home: &std::path::Path,
    config: &CodeyConfig,
    handler: &codey_runtime_core::bridge::BridgeHandler,
    injection_scripts: &cdp::PreparedInjectionScripts,
    storage: StartupStorageState,
    patch: &StartupPatchState,
    runtime_config_overrides: &[String],
    workflow_proxy: Option<&crate::codex_startup_patch::WorkflowProxyLaunchConfig>,
) -> Result<SpawnedRenderer> {
    let mut spawned = match spawn_codex(
        &storage.app_dir,
        patch.debug_port,
        config.slim_codex_pet,
        config.fast_codex_startup,
        config.subagent_optimization,
        config.gpu_launch_mode,
        runtime_config_overrides,
        workflow_proxy,
    )
    .await
    {
        Ok(spawned) => spawned,
        Err(error) => {
            return Err(restore_runtime_config_after_error(home, error).await);
        }
    };
    let maintenance = MaintenanceStatus {
        session_status: storage.session_maintenance.status,
        session_files_fixed: storage.session_maintenance.files_fixed,
        sqlite_rows_updated: storage.session_maintenance.sqlite_rows_updated,
        ghost_tasks_pruned: storage.session_maintenance.ghost_tasks_pruned,
        performance_status: spawned.performance_status.clone(),
        performance_detail: spawned.performance_detail.clone(),
    };
    let child = Arc::new(Mutex::new(spawned.child.take()));
    let injected_target = inject_initial_renderer(
        patch.debug_port,
        handler.clone(),
        injection_scripts,
        &storage.app_dir,
        home,
        &spawned,
        &child,
    )
    .await?;
    Ok(SpawnedRenderer {
        app_dir: storage.app_dir,
        spawned,
        child,
        maintenance,
        injected_target,
    })
}

fn spawn_runtime_watchers(inputs: RuntimeWatcherInputs) -> RuntimeWatchers {
    let RuntimeWatcherInputs {
        injected_target,
        debug_port,
        handler,
        injection_scripts,
        child,
        process_id,
        protect_crashpad_pending,
        crashpad_pending_stats,
    } = inputs;
    #[cfg(not(windows))]
    let _ = process_id;
    let InjectionWatchdog {
        statuses: injection_statuses,
        websocket_url: injection_websocket_url,
        shutdown: watchdog_shutdown,
        task: watchdog_task,
    } = spawn_injection_watchdog(injected_target, debug_port, handler, injection_scripts);
    let codex_exited = Arc::new(AtomicBool::new(false));
    let crashpad_guard_enabled = Arc::new(AtomicBool::new(protect_crashpad_pending));
    let (crashpad_guard_shutdown, crashpad_guard_task) =
        spawn_crashpad_guard_watcher(crashpad_guard_enabled.clone(), crashpad_pending_stats);
    #[cfg(windows)]
    let (exit_watchdog_shutdown, codex_exit, exit_watchdog_task) =
        spawn_codex_exit_watcher(child, process_id, codex_exited);
    #[cfg(not(windows))]
    let (exit_watchdog_shutdown, codex_exit, exit_watchdog_task) =
        spawn_codex_exit_watcher(child, codex_exited);
    RuntimeWatchers {
        injection_statuses,
        injection_websocket_url,
        watchdog_shutdown,
        watchdog_task,
        crashpad_guard_enabled,
        crashpad_guard_shutdown,
        crashpad_guard_task,
        exit_watchdog_shutdown,
        exit_watchdog_task,
        codex_exit,
    }
}

impl CodeyRuntime {
    pub async fn renderer_websocket_url(&self) -> Arc<str> {
        self.injection_websocket_url.read().await.clone()
    }

    pub async fn applied_model_config(&self) -> RuntimeModelConfig {
        self.applied_model_config.read().await.clone()
    }

    pub async fn mark_model_config_applied(&self, config: &CodeyConfig) {
        *self.applied_model_config.write().await = RuntimeModelConfig::from_config(config);
    }

    pub async fn applied_subagent_config(&self) -> RuntimeSubagentConfig {
        self.applied_subagent_config.read().await.clone()
    }

    pub async fn mark_subagent_config_applied(&self, config: &CodeyConfig) {
        *self.applied_subagent_config.write().await = RuntimeSubagentConfig::from_config(config);
    }

    pub fn supports_subagent_config_hot_reload(&self, config: &CodeyConfig) -> bool {
        self.applied_config.subagent_optimization
            && config.subagent_optimization
            && self.applied_config.fast_context_tools == config.fast_context_tools
            && self.applied_config.active_profile() == config.active_profile()
    }

    pub fn set_crashpad_pending_protection(&self, enabled: bool) {
        self.crashpad_guard_enabled
            .store(enabled, Ordering::Release);
    }

    pub fn injection_statuses_for_display(
        &self,
        statuses: Arc<[cdp::InjectionScriptStatus]>,
    ) -> Arc<[cdp::InjectionScriptStatus]> {
        statuses
    }

    pub async fn refresh_injection_statuses(&self) -> Arc<[cdp::InjectionScriptStatus]> {
        let websocket_url = self.injection_websocket_url.read().await.clone();
        let statuses = cdp::read_injection_statuses(&websocket_url, &self.injection_scripts)
            .await
            .unwrap_or_else(|error| {
                self.injection_scripts
                    .statuses_with_error(format!("实时生效自检失败：{error:#}"))
            });
        if self.injection_websocket_url.read().await.as_ref() != websocket_url.as_ref() {
            let statuses = self.injection_statuses.read().await.clone();
            return self.injection_statuses_for_display(statuses);
        }
        let statuses = self.injection_statuses_for_display(statuses);
        *self.injection_statuses.write().await = statuses.clone();
        statuses
    }

    pub async fn start(
        config: &CodeyConfig,
        handler: codey_runtime_core::bridge::BridgeHandler,
        crashpad_pending_stats: CrashpadPendingStatsHandle,
        workflow_proxy: Option<&crate::codex_startup_patch::WorkflowProxyLaunchConfig>,
    ) -> Result<(Self, oneshot::Receiver<()>, Option<oneshot::Receiver<()>>)> {
        let home = codex_home();
        let injection_scripts = cdp::prepare_injection_scripts(
            config.fast_codex_startup,
            config.slim_codex_pet,
            config.hide_full_access_warning,
            &config.user_scripts,
        );
        let initial_storage_guards = spawn_initial_storage_guards(&home, config);
        let route = resolve_startup_route_context(&home, config).await?;
        let storage = prepare_startup_storage(
            &home,
            config,
            &route.original_provider,
            initial_storage_guards,
            &crashpad_pending_stats,
        )
        .await?;
        let PreparedProviderState {
            protocol_proxy,
            applied_route_files,
            runtime_config,
            runtime_config_overrides,
        } = prepare_runtime_provider_state(&home, config, &route).await?;
        let patch =
            prepare_startup_patches_and_overlay(&home, config, applied_route_files.as_ref())
                .await?;
        let SpawnedRenderer {
            app_dir,
            spawned,
            child,
            maintenance,
            injected_target,
        } = spawn_and_inject_runtime(
            &home,
            config,
            &handler,
            &injection_scripts,
            storage,
            &patch,
            &runtime_config_overrides,
            workflow_proxy,
        )
        .await?;
        restore_cc_switch_provider_after_startup(&home, &route).await;
        #[cfg(target_os = "macos")]
        let inspector_argument = spawned.inspector_argument.clone();
        let process_id = spawned.process_id;
        let RuntimeWatchers {
            injection_statuses,
            injection_websocket_url,
            watchdog_shutdown,
            watchdog_task,
            crashpad_guard_enabled,
            crashpad_guard_shutdown,
            crashpad_guard_task,
            exit_watchdog_shutdown,
            exit_watchdog_task,
            codex_exit,
        } = spawn_runtime_watchers(RuntimeWatcherInputs {
            injected_target,
            debug_port: patch.debug_port,
            handler,
            injection_scripts: injection_scripts.clone(),
            child: child.clone(),
            process_id,
            protect_crashpad_pending: config.protect_crashpad_pending,
            crashpad_pending_stats,
        });
        Ok((
            Self {
                codex_app_path: app_dir,
                maintenance,
                applied_model_config: RwLock::new(RuntimeModelConfig::from_config(&runtime_config)),
                applied_subagent_config: RwLock::new(RuntimeSubagentConfig::from_config(
                    &runtime_config,
                )),
                applied_config: runtime_config,
                injection_statuses,
                injection_scripts,
                injection_websocket_url,
                child,
                process_id,
                #[cfg(unix)]
                process_group_id: spawned.process_group_id,
                #[cfg(target_os = "macos")]
                inspector_argument,
                watchdog_shutdown: Mutex::new(Some(watchdog_shutdown)),
                watchdog_task: Mutex::new(Some(watchdog_task)),
                route_overlay_shutdown: Mutex::new(patch.route_overlay_shutdown),
                route_overlay_task: Mutex::new(patch.route_overlay_task),
                exit_watchdog_shutdown: Mutex::new(Some(exit_watchdog_shutdown)),
                exit_watchdog_task: Mutex::new(Some(exit_watchdog_task)),
                crashpad_guard_enabled,
                crashpad_guard_shutdown: Mutex::new(Some(crashpad_guard_shutdown)),
                crashpad_guard_task: Mutex::new(Some(crashpad_guard_task)),
                protocol_proxy: Mutex::new(protocol_proxy),
            },
            codex_exit,
            patch.route_changed,
        ))
    }

    pub async fn stop(&self) -> Result<()> {
        stop_runtime_watcher(
            &self.crashpad_guard_shutdown,
            &self.crashpad_guard_task,
            "cleanup_failed",
            "stop_crashpad_pending_guard",
            "Crashpad 磁盘保护任务关闭失败",
        )
        .await;
        stop_runtime_watcher(
            &self.route_overlay_shutdown,
            &self.route_overlay_task,
            "route_overlay_watch_failed",
            "stop_cc_switch_route_overlay_watcher",
            "CC Switch 路由配置监听器关闭失败",
        )
        .await;
        stop_runtime_watcher(
            &self.watchdog_shutdown,
            &self.watchdog_task,
            "injection_watchdog_failed",
            "stop_cdp_watchdog",
            "Codey CDP watchdog 关闭失败",
        )
        .await;
        stop_runtime_watcher(
            &self.exit_watchdog_shutdown,
            &self.exit_watchdog_task,
            "process_watch_failed",
            "stop_codex_exit_watcher",
            "Codex 退出监听器关闭失败",
        )
        .await;
        let process_stop = stop_codex_processes(
            &self.codex_app_path,
            self.process_id,
            #[cfg(unix)]
            self.process_group_id,
            #[cfg(target_os = "macos")]
            self.inspector_argument.as_deref(),
        )
        .await;

        if let Some(child) = self.child.lock().await.take() {
            reap_child_after_cleanup(child, "reap_child_during_runtime_stop").await;
        }
        let protocol_proxy_stop = if let Some(proxy) = self.protocol_proxy.lock().await.take() {
            proxy.shutdown().await
        } else {
            Ok(())
        };
        let config_restore = restore_runtime_config(&codex_home()).await;
        if let Err(error) = &process_stop {
            error_log::record_failure(
                "cleanup_failed",
                "stop_codex_processes",
                format!("{error:#}"),
                serde_json::json!({
                    "appPath": self.codex_app_path,
                    "processId": self.process_id,
                }),
            );
        }
        if let Err(error) = &protocol_proxy_stop {
            error_log::record_failure(
                "cleanup_failed",
                "stop_chat_completions_protocol_proxy",
                format!("{error:#}"),
                serde_json::json!({}),
            );
        }
        let mut failures = Vec::new();
        if let Err(error) = process_stop {
            failures.push(format!("清理 Codex 遗留进程失败：{error:#}"));
        }
        if let Err(error) = protocol_proxy_stop {
            failures.push(format!("关闭本地协议代理失败：{error:#}"));
        }
        if let Err(error) = config_restore {
            failures.push(format!("恢复 Codex 配置失败：{error:#}"));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(failures.join("；"))
        }
    }
}

fn preserve_cc_switch_route(state: RouteTakeoverState) -> Result<bool> {
    if state.managed && !state.live {
        anyhow::bail!(
            "检测到 CC Switch 已开启 Codex 路由，但当前 Live 配置未处于接管状态。\
             为避免 Codey 覆盖路由，已停止启动；请在 CC Switch 中关闭并重新开启 Codex 路由后重试"
        );
    }
    Ok(state.live)
}

async fn restore_cc_switch_provider_after_startup(
    home: &std::path::Path,
    route: &StartupRouteContext,
) {
    if route.preserve_provider_route
        || route.current_profile.protocol != RelayProtocol::ChatCompletions
        || route.current_profile.cc_switch_provider_id.is_none()
    {
        return;
    }
    let home = home.to_path_buf();
    let restored =
        tokio::task::spawn_blocking(move || restore_runtime_cc_switch_provider_config(&home))
            .await
            .map_err(|error| {
                anyhow::Error::new(error).context("还原 CC Switch Provider 任务异常退出")
            })
            .and_then(|result| result);
    if let Err(error) = restored {
        error_log::record_failure(
            "restore_failed",
            "restore_cc_switch_provider_after_startup",
            format!("{error:#}"),
            serde_json::json!({
                "provider": route.original_provider,
            }),
        );
        eprintln!("Codex 启动后还原 CC Switch Provider 失败：{error:#}");
    }
}

fn spawn_crashpad_guard_watcher(
    enabled: Arc<AtomicBool>,
    stats: CrashpadPendingStatsHandle,
) -> (oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(crashpad_pending_guard::GUARD_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                _ = interval.tick() => {}
            }
            if !enabled.load(Ordering::Acquire) {
                continue;
            }
            let result =
                tokio::task::spawn_blocking(crashpad_pending_guard::enforce_system_limit).await;
            match result {
                Ok(run) => {
                    if !run.cleanup.errors.is_empty() || run.cleanup.still_over_limit {
                        error_log::record_failure_async(
                            "cleanup_failed",
                            "enforce_crashpad_pending_limit",
                            if run.cleanup.still_over_limit {
                                "Crashpad pending 仍超过安全上限".to_string()
                            } else {
                                format!(
                                    "{} 个 Crashpad 待处理文件未能完成收敛",
                                    run.cleanup.errors.len()
                                )
                            },
                            serde_json::json!({
                                "errorCount": run.cleanup.errors.len(),
                                "stillOverLimit": run.cleanup.still_over_limit,
                                "bytesReclaimed": run.cleanup.bytes_reclaimed,
                            }),
                        )
                        .await;
                    }
                    let _ = stats.replace_if_idle(run.snapshot);
                }
                Err(error) => {
                    error_log::record_failure_async(
                        "cleanup_failed",
                        "enforce_crashpad_pending_limit",
                        error.to_string(),
                        serde_json::json!({
                            "taskJoinFailed": true,
                        }),
                    )
                    .await;
                }
            }
        }
    });
    (shutdown_tx, task)
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
) -> SessionMaintenanceSummary {
    let pruned_entries = match index_cleanup {
        Ok(report) => report.pruned_entries,
        Err(_) => 0,
    };
    let has_errors = provider_sync.status != ProviderSyncStatus::Synced
        || !provider_sync.skipped_locked_rollout_files.is_empty()
        || index_cleanup.is_err();
    let status = if has_errors { "error" } else { "ready" };
    SessionMaintenanceSummary {
        status: status.to_string(),
        files_fixed: provider_sync.changed_session_files,
        sqlite_rows_updated: provider_sync.sqlite_rows_updated,
        ghost_tasks_pruned: pruned_entries,
    }
}

#[cfg(test)]
mod maintenance_status_tests;

pub async fn restore_previous_runtime_state(home: &std::path::Path) -> Result<()> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || restore_previous_runtime_state_blocking(&home))
        .await
        .context("恢复上次 Codey 运行状态任务异常退出")?
}

fn restore_previous_runtime_state_blocking(home: &std::path::Path) -> Result<()> {
    let provider_result = provider_lease::restore_legacy();
    let config_result = restore_runtime_provider_config(home);
    if let Err(error) = &provider_result {
        error_log::record_failure(
            "restore_failed",
            "restore_legacy_provider_lease",
            format!("{error:#}"),
            serde_json::json!({}),
        );
    }
    if let Err(error) = &config_result {
        error_log::record_failure(
            "restore_failed",
            "restore_runtime_provider_config",
            format!("{error:#}"),
            serde_json::json!({
                "codexHome": home,
            }),
        );
    }
    match (provider_result, config_result) {
        (Ok(_), Ok(_)) => Ok(()),
        (Err(provider), Ok(_)) => Err(provider).context("恢复会话 provider 失败"),
        (Ok(_), Err(config)) => Err(config).context("恢复 Codex 配置失败"),
        (Err(provider), Err(config)) => {
            anyhow::bail!("恢复会话 provider 失败：{provider}；恢复 Codex 配置也失败：{config}")
        }
    }
}

pub async fn restore_runtime_config(home: &std::path::Path) -> Result<()> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || restore_runtime_config_blocking(&home))
        .await
        .context("恢复 Codey 运行时配置任务异常退出")?
}

fn restore_runtime_config_blocking(home: &std::path::Path) -> Result<()> {
    let result = restore_runtime_provider_config(home)
        .map(|_| ())
        .context("恢复 Codex 配置失败");
    if let Err(error) = &result {
        error_log::record_failure(
            "restore_failed",
            "restore_runtime_provider_config",
            format!("{error:#}"),
            serde_json::json!({
                "codexHome": home,
            }),
        );
    }
    result
}

async fn restore_runtime_config_after_error(
    home: &std::path::Path,
    error: anyhow::Error,
) -> anyhow::Error {
    match restore_runtime_config(home).await {
        Ok(()) => error,
        Err(restore_error) => {
            anyhow::anyhow!("{error:#}；启动失败后恢复临时 Codex 配置也失败：{restore_error:#}")
        }
    }
}

#[cfg(test)]
mod gpu_launch_argument_tests;

#[cfg(all(test, unix))]
mod tests;
