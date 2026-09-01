#[cfg(all(test, unix))]
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result};
use codey_runtime_core::app_paths::resolve_codex_app_dir_with_saved;
use codey_runtime_core::config_manager::ConfigManager;
use codey_runtime_core::launcher::build_codex_command;
use codey_runtime_data::{ProviderSyncResult, ProviderSyncStatus};
use serde::Serialize;
use tokio::process::Child;
#[cfg(not(windows))]
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, oneshot};

use crate::cdp;
use crate::codex_config::{
    BUILTIN_OPENAI_PROVIDER_ID, RuntimeRouterConfigOptions, apply_runtime_router_config,
    codex_home, prepare_persistent_router_resume_shim as prepare_codex_router_resume_shim,
    restore_runtime_config_for_router_mode as restore_codex_runtime_config_for_router_mode,
    user_owned_router_provider_occupies_id,
};
use crate::config::{
    CodeyConfig, GpuLaunchMode, ProviderProfile, RouteRequestLogConfig, RuntimeModelTarget,
};
use crate::crashpad_pending_guard::{self, CrashpadPendingStatsHandle};
use crate::error_log;
use crate::local_router::{self, LocalRouter, ROUTER_PROVIDER_ID, RuntimeRouterEndpoint};
use crate::maintenance_lock;
use crate::message_delete;
use crate::model_catalog;
use crate::model_id;
use crate::pet_slim_patch;
use crate::route_request_log::RouteRequestLogReconfigure;
use crate::session_index_cleanup::{self, SessionIndexCleanupReport};
use crate::startup_maintenance::{self, ProviderSyncPlan};
use crate::subagent_policy;
use crate::trace_log_guard;

mod platform;
mod process;

use platform::*;
use process::{
    SpawnedCodex, prepare_codex_for_launch, reap_child_after_cleanup, spawn_codex,
    spawn_codex_exit_watcher,
};
#[cfg(test)]
use process::{codex_runtime_arguments, gpu_launch_arguments};

const CDP_WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
const CDP_WATCHDOG_FAILURE_THRESHOLD: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InjectionHealth {
    Healthy,
    Unhealthy,
    Inconclusive,
    TargetUnavailable,
}
pub const CODEX_APP_NOT_FOUND_ERROR: &str = "找不到 Codex 桌面应用";
pub const CODEX_APP_PATH_INVALID_ERROR: &str = "配置的 Codex App 路径无效或指向了 Codex CLI；请选择 Codex 桌面 App，不要选择 codex.exe 命令行程序";
const DISABLE_GPU_ARGUMENT: &str = "--disable-gpu";
const DISABLE_GPU_RASTERIZATION_ARGUMENT: &str = "--disable-gpu-rasterization";
const DISABLE_BACKGROUND_ECOQOS_ARGUMENT: &str = "--disable-features=UseEcoQoSForBackgroundProcess";
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
    routes: Vec<(String, String, bool, bool)>,
    selected_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    manual_third_party_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    declared_official_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    upstream_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    default_model: String,
}

impl RuntimeModelConfig {
    pub fn from_config(config: &CodeyConfig) -> Self {
        Self {
            routes: config
                .profiles
                .iter()
                .map(|profile| {
                    (
                        profile.provider_id().to_string(),
                        profile.name.clone(),
                        profile.official_account,
                        profile.supports_auto_review,
                    )
                })
                .collect(),
            selected_models_by_provider: config.selected_models_by_provider.clone(),
            manual_third_party_models_by_provider: config
                .manual_third_party_models_by_provider
                .clone(),
            declared_official_models_by_provider: config
                .declared_official_models_by_provider
                .clone(),
            upstream_models_by_provider: config.upstream_models_by_provider.clone(),
            default_model: config.default_model.clone(),
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
    exit_watchdog_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    exit_watchdog_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    crashpad_guard_enabled: Arc<AtomicBool>,
    crashpad_guard_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    crashpad_guard_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    local_router: Option<LocalRouter>,
}

fn persistent_session_provider(home: &std::path::Path) -> Result<String> {
    let config_path = home.join("config.toml");
    let snapshot = ConfigManager::new(&config_path)
        .load()
        .context("读取 Codex 持久配置失败")?;
    let document = snapshot.document();
    if user_owned_router_provider_occupies_id(document) {
        // Runtime setup rejects a user-owned collision too, but session
        // maintenance is permanent and therefore must not run before the
        // same validation. Codey-owned resume shims are not occupancy.
        anyhow::bail!(
            "Codex config.toml 已占用 Codey 内部 Provider ID「{}」；请先重命名该自定义 Provider",
            ROUTER_PROVIDER_ID
        );
    }
    let provider = document
        .get("model_provider")
        .and_then(toml_edit::Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or(BUILTIN_OPENAI_PROVIDER_ID);
    if provider == ROUTER_PROVIDER_ID {
        // Legacy releases could leave the launch-only router id selected in
        // the persistent config; thread records must never sync toward it.
        return Ok(BUILTIN_OPENAI_PROVIDER_ID.to_string());
    }
    Ok(provider.to_string())
}

async fn resolve_persistent_session_provider(home: &std::path::Path) -> Result<String> {
    let provider_home = home.to_path_buf();
    tokio::task::spawn_blocking(move || persistent_session_provider(&provider_home))
        .await
        .map_err(|error| {
            let error = anyhow::Error::new(error).context("读取持久会话 Provider 任务异常退出");
            error_log::record_failure(
                "patch_failed",
                "read_persistent_session_provider",
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
                "read_persistent_session_provider",
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
    provider: Option<&str>,
) -> Result<SessionMaintenanceSummary> {
    let maintenance_home = home.to_path_buf();
    let maintenance_provider = provider.map(ToString::to_string);
    let maintenance_result = tokio::task::spawn_blocking(move || {
        let stale_lock_recovery = maintenance_lock::recover_stale_locks(&maintenance_home);
        let provider_sync = maintenance_provider.map(|maintenance_provider| {
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
            }
        });
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
    if let Some(provider_sync) = provider_sync.as_ref() {
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
    Ok(session_maintenance_summary(
        provider_sync.as_ref(),
        &index_cleanup,
    ))
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

struct StartupModelCatalog {
    use_official_catalog: bool,
    model_state: model_catalog::ModelSelectionState,
}

struct PreparedCodexStartupState {
    runtime_config: CodeyConfig,
    runtime_config_overrides: Vec<String>,
}

fn route_subagent_model(
    route_provider: &str,
    model: &str,
    targets: &[RuntimeModelTarget],
    builtin_official_catalog: bool,
) -> String {
    let requested = model.trim();
    let target = targets
        .iter()
        .find(|target| model_id::equal(&target.alias, requested))
        .or_else(|| {
            let mut matches = targets
                .iter()
                .filter(|target| model_id::equal(&target.upstream_model, requested));
            let target = matches.next()?;
            matches.next().is_none().then_some(target)
        })
        .or_else(|| {
            targets.iter().find(|target| {
                target.provider_id == route_provider
                    && model_id::equal(&target.upstream_model, requested)
            })
        });
    if let Some(target) = target {
        // The built-in official catalog has native slugs. Synthetic aliases
        // would lose its model metadata, including prefer_websockets.
        return if builtin_official_catalog && target.official {
            target.upstream_model.clone()
        } else {
            target.alias.clone()
        };
    }
    if builtin_official_catalog {
        requested.to_string()
    } else {
        local_router::model_alias(route_provider, requested)
    }
}

fn should_install_codey_model_catalog(official_only: bool, catalog_available: bool) -> bool {
    !official_only && catalog_available
}

fn runtime_default_model(
    config: &CodeyConfig,
    codey_catalog_installed: bool,
    model_state: &model_catalog::ModelSelectionState,
) -> Option<String> {
    let model = if codey_catalog_installed {
        // The generated catalog uses route-qualified selector ids. Keep that
        // stable id inside Codex so its picker can resolve the configured
        // default; the loopback gateway alone translates it back to the
        // upstream model id immediately before forwarding the HTTP request.
        config.default_model().unwrap_or(&model_state.default_model)
    } else {
        // The built-in Codex catalog only contains native OpenAI model ids.
        &model_state.default_model
    };
    let model = model.trim();
    (!model.is_empty()).then(|| model.to_string())
}

async fn prepare_startup_model_catalog(
    config: &CodeyConfig,
    current_profile: &ProviderProfile,
    home: &std::path::Path,
) -> Result<StartupModelCatalog> {
    let catalog_home = home.to_path_buf();
    let official_provider =
        current_profile.official_account && config.official_account_available_this_launch;
    let has_third_party_route = config.has_third_party_route();
    let (runtime_upstream_models, runtime_selected_models) = config.runtime_catalog_models();
    let runtime_websocket_models = config.runtime_websocket_model_aliases();
    let runtime_native_web_search_models = config.runtime_native_web_search_model_aliases();
    let refresh_official_provider =
        config.official_account_available_this_launch && !has_third_party_route;
    let refresh_upstream_models = has_third_party_route.then_some(runtime_upstream_models);
    let current_provider_id = current_profile.provider_id();
    let upstream_models = config
        .upstream_models_by_provider
        .get(current_provider_id)
        .cloned();
    let selected_models = if official_provider {
        config
            .selected_models_by_provider
            .get(current_provider_id)
            .cloned()
            .unwrap_or_default()
    } else {
        config.enabled_route_models(current_provider_id)
    };
    let manual_models = config
        .manual_third_party_models_by_provider
        .get(current_provider_id)
        .cloned()
        .unwrap_or_default();
    let requested_default_model = config.default_model_for_profile(current_profile);
    let (refresh_result, cached_catalog_result, selection_result) =
        tokio::task::spawn_blocking(move || {
            let refresh = model_catalog::refresh_for_provider_with_capabilities(
                &catalog_home,
                refresh_official_provider,
                refresh_upstream_models.as_deref(),
                &runtime_selected_models,
                &runtime_websocket_models,
                &runtime_native_web_search_models,
            );
            let cached_catalog = if refresh.is_err() {
                model_catalog::prepare_cached_catalog_for_native_web_search(
                    &catalog_home,
                    &runtime_native_web_search_models,
                )
            } else {
                Ok(false)
            };
            let selection = model_catalog::selection_state_with_manual_models(
                &catalog_home,
                official_provider,
                upstream_models.as_deref(),
                &selected_models,
                &manual_models,
                requested_default_model.as_deref(),
            );
            (refresh, cached_catalog, selection)
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

    let catalog_available = match cached_catalog_result {
        Ok(available) => available,
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "sanitize_cached_model_catalog",
                format!("{error:#}"),
                serde_json::json!({
                    "fallback": "codex_builtin_catalog",
                    "officialProvider": official_provider,
                }),
            );
            eprintln!("旧模型目录的原生网页搜索能力无法安全校正，改用 Codex 内置目录：{error:#}");
            false
        }
    };
    let catalog_available_for_runtime = match refresh_result {
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
    // Official OpenAI routes should inherit Codex's built-in model metadata,
    // including its context window and automatic-compaction defaults. Codey's
    // generated catalog remains necessary for third-party model filtering and
    // synthetic model entries.
    let use_official_catalog =
        should_install_codey_model_catalog(!has_third_party_route, catalog_available_for_runtime);
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
    local_router: &RuntimeRouterEndpoint,
    startup_catalog: StartupModelCatalog,
) -> Result<PreparedCodexStartupState> {
    let StartupModelCatalog {
        use_official_catalog,
        model_state,
    } = startup_catalog;
    let runtime_config_home = home.to_path_buf();
    let runtime_local_router = local_router.clone();
    let router_route_provider = current_profile.provider_id().to_string();
    let runtime_default_model = runtime_default_model(config, use_official_catalog, &model_state);
    let fast_context_tools = config.fast_context_tools;
    let mut runtime_subagent_config = config.clone();
    runtime_subagent_config.active_profile_id = current_profile.id.clone();
    subagent_policy::reconcile_with_model_state(&mut runtime_subagent_config, Some(&model_state));
    let route_model_targets = runtime_subagent_config.runtime_model_targets();
    let builtin_official_catalog =
        !use_official_catalog && !runtime_subagent_config.has_third_party_route();
    let subagent_optimization = runtime_subagent_config.subagent_optimization;
    let subagent_model = route_subagent_model(
        &router_route_provider,
        &runtime_subagent_config.subagent_model,
        &route_model_targets,
        builtin_official_catalog,
    );
    let subagent_reasoning_effort = runtime_subagent_config.subagent_reasoning_effort.clone();
    let mut subagent_roles = runtime_subagent_config.subagent_roles.clone();
    for selection in subagent_roles.values_mut() {
        selection.model = route_subagent_model(
            &router_route_provider,
            &selection.model,
            &route_model_targets,
            builtin_official_catalog,
        );
    }
    let runtime_config = tokio::task::spawn_blocking(move || {
        apply_runtime_router_config(
            &runtime_config_home,
            RuntimeRouterConfigOptions {
                local_router: Some(&runtime_local_router),
                use_official_catalog,
                default_model: runtime_default_model.as_deref(),
                fast_context_tools,
                subagent_optimization,
                subagent_model: &subagent_model,
                subagent_reasoning_effort: &subagent_reasoning_effort,
                subagent_roles: Some(&subagent_roles),
            },
        )
    })
    .await
    .map_err(|error| {
        let error = anyhow::Error::new(error).context("应用运行时 Provider 配置任务异常退出");
        error_log::record_failure(
            "patch_failed",
            "apply_runtime_router_config",
            format!("{error:#}"),
            serde_json::json!({
                "profile": current_profile.name,
                "provider": ROUTER_PROVIDER_ID,
                "fastContextTools": config.fast_context_tools,
                "subagentOptimization": config.subagent_optimization,
                "taskJoinFailed": true,
            }),
        );
        error
    })?;
    let applied = runtime_config.map_err(|error| {
        error_log::record_failure(
            "patch_failed",
            "apply_runtime_router_config",
            format!("{error:#}"),
            serde_json::json!({
                "profile": current_profile.name,
                "provider": ROUTER_PROVIDER_ID,
                "fastContextTools": config.fast_context_tools,
                "subagentOptimization": config.subagent_optimization,
            }),
        );
        error
    })?;
    runtime_subagent_config.fast_context_tools = applied.fast_context_tools_active;
    Ok(PreparedCodexStartupState {
        runtime_config: runtime_subagent_config,
        runtime_config_overrides: applied.runtime_config_overrides,
    })
}

async fn await_initial_storage_guards(
    initial_trace_guard: tokio::task::JoinHandle<Result<trace_log_guard::TraceLogGuardReport>>,
    disable_trace_log_writes: bool,
    trace_log_write_protection_active: &AtomicBool,
    initial_crashpad_guard: tokio::task::JoinHandle<crashpad_pending_guard::CrashpadGuardRun>,
    protect_crashpad_pending: bool,
    crashpad_pending_stats: &CrashpadPendingStatsHandle,
) -> Result<()> {
    match initial_trace_guard.await {
        Ok(Ok(report)) => trace_log_write_protection_active.store(
            report.protection_active(disable_trace_log_writes),
            Ordering::Release,
        ),
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

struct RuntimeConfigRestoreContext<'a> {
    home: &'a std::path::Path,
    local_router_enabled: bool,
}

async fn inject_initial_renderer(
    debug_port: u16,
    handler: codey_runtime_core::bridge::BridgeHandler,
    injection_scripts: &cdp::PreparedInjectionScripts,
    app_dir: &std::path::Path,
    spawned: &SpawnedCodex,
    child: &Arc<Mutex<Option<Child>>>,
    restore_context: RuntimeConfigRestoreContext<'_>,
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
    Err(restore_runtime_config_after_error(
        restore_context.home,
        restore_context.local_router_enabled,
        error,
    )
    .await)
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
            let health = tokio::select! {
                biased;
                _ = &mut shutdown_rx => break 'watchdog,
                result = cdp::is_target_healthy(target.websocket_url()) => {
                    match result {
                        Ok(cdp::TargetHealth::Healthy) => InjectionHealth::Healthy,
                        Ok(cdp::TargetHealth::Unhealthy) => InjectionHealth::Unhealthy,
                        Ok(cdp::TargetHealth::Busy) => {
                            // The renderer answered CDP but the in-page bridge
                            // round-trip missed its budget: the bridge is still
                            // installed, the page is just busy. Reinjecting
                            // would pile more script work onto a stalled page.
                            InjectionHealth::Inconclusive
                        }
                        Err(error) => {
                            let requires_rediscovery =
                                cdp::target_health_error_requires_rediscovery(&error);
                            error_log::record_failure_async(
                                "injection_health_check_failed",
                                "check_cdp_bridge_health",
                                format!("{error:#}"),
                                serde_json::json!({
                                    "websocketUrl": target.websocket_url(),
                                    "requiresTargetRediscovery": requires_rediscovery,
                                }),
                            )
                            .await;
                            if requires_rediscovery {
                                // The saved /devtools/page endpoint no longer
                                // accepts CDP traffic. Rediscover immediately;
                                // retrying this URL cannot repair a replaced
                                // Windows renderer target.
                                InjectionHealth::TargetUnavailable
                            } else {
                                // A busy renderer can miss the diagnostic
                                // deadline while its bridge remains installed.
                                // Reinjecting in that state adds more CDP/script
                                // work to an already stalled page.
                                InjectionHealth::Inconclusive
                            }
                        }
                    }
                }
            };
            if !watchdog_should_reinject(&mut consecutive_failures, health) {
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

struct StartupStorageState {
    app_dir: PathBuf,
    session_maintenance: SessionMaintenanceSummary,
}

struct PreparedProviderState {
    runtime_config: CodeyConfig,
    runtime_config_overrides: Vec<String>,
}

struct StartupPatchState {
    debug_port: u16,
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

fn resolve_startup_profile(config: &CodeyConfig) -> Result<ProviderProfile> {
    let current_profile = config
        .effective_runtime_default_target()
        .and_then(|target| {
            config
                .profiles
                .iter()
                .find(|profile| profile.id == target.route_id)
                .cloned()
        })
        .or_else(|| config.active_profile())
        .ok_or_else(|| anyhow::anyhow!("找不到全局默认模型所属的 Codex 线路"))?;
    if current_profile.official_account && !config.official_account_available_this_launch {
        anyhow::bail!("当前线路需要官方账号登录，但本次 Codex 启动未检测到可用的官方登录态");
    }
    current_profile.validate().map_err(anyhow::Error::msg)?;
    Ok(current_profile)
}

async fn prepare_startup_storage(
    home: &std::path::Path,
    config: &CodeyConfig,
    session_provider_sync_target: Option<&str>,
    guards: InitialStorageGuards,
    trace_log_write_protection_active: &AtomicBool,
    crashpad_pending_stats: &CrashpadPendingStatsHandle,
) -> Result<StartupStorageState> {
    let app_dir = resolve_configured_codex_app_dir(config).await?;
    // Session repair must never race a live Codex writer. Stopping the old
    // runtime first also gives SQLite and rollout buffers a chance to flush
    // before any permanent maintenance is applied.
    prepare_codex_for_launch(&app_dir).await?;

    // Permanent maintenance runs before Codey installs the temporary runtime
    // provider override. A lightweight header/SQLite validation normally
    // reuses the last successful provider sync; provider changes still
    // fall back to the complete rollout and SQLite repair.
    let session_maintenance =
        run_startup_session_maintenance(home, session_provider_sync_target).await?;
    await_initial_storage_guards(
        guards.trace,
        config.disable_trace_log_writes,
        trace_log_write_protection_active,
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
    current_profile: &ProviderProfile,
    local_router: &LocalRouter,
) -> Result<PreparedProviderState> {
    let startup_catalog = prepare_startup_model_catalog(config, current_profile, home).await?;
    let router_endpoint = local_router.endpoint();
    let prepared_startup = prepare_codex_startup_state(
        config,
        current_profile,
        home,
        &router_endpoint,
        startup_catalog,
    )
    .await?;
    Ok(PreparedProviderState {
        runtime_config: prepared_startup.runtime_config,
        runtime_config_overrides: prepared_startup.runtime_config_overrides,
    })
}

fn native_subagent_model(
    config: &CodeyConfig,
    targets: &[RuntimeModelTarget],
    model: &str,
) -> String {
    let model = model.trim();
    targets
        .iter()
        .find(|target| model_id::equal(&target.alias, model))
        .map(|target| target.upstream_model.clone())
        .or_else(|| native_provider_prefixed_subagent_model(config, model))
        .unwrap_or_else(|| model.to_string())
}

fn native_provider_prefixed_subagent_model(config: &CodeyConfig, model: &str) -> Option<String> {
    for profile in &config.profiles {
        let provider_id = profile.provider_id();
        let prefix = local_router::model_alias(provider_id, "");
        let Some(upstream_model) = strip_model_provider_prefix(model, &prefix) else {
            continue;
        };
        let known_model = config
            .upstream_models_by_provider
            .get(provider_id)
            .into_iter()
            .flatten()
            .chain(
                config
                    .selected_models_by_provider
                    .get(provider_id)
                    .into_iter()
                    .flatten(),
            )
            .chain(
                config
                    .declared_official_models_by_provider
                    .get(provider_id)
                    .into_iter()
                    .flatten(),
            )
            .find(|known| model_id::equal(known, upstream_model))
            .cloned()
            .or_else(|| {
                model_catalog::default_official_model_slugs()
                    .into_iter()
                    .find(|known| model_id::equal(known, upstream_model))
            });
        if known_model.is_some() {
            return known_model;
        }
    }
    None
}

fn strip_model_provider_prefix<'a>(model: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.trim();
    model
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .and_then(|_| model.get(prefix.len()..))
        .map(str::trim)
        .filter(|suffix| !suffix.is_empty())
}

fn native_subagent_runtime_config(config: &CodeyConfig) -> CodeyConfig {
    let mut runtime_config = config.clone();
    let targets = config.runtime_model_targets();
    runtime_config.subagent_model = native_subagent_model(config, &targets, &config.subagent_model);
    for selection in runtime_config.subagent_roles.values_mut() {
        selection.model = native_subagent_model(config, &targets, &selection.model);
    }
    runtime_config
}

async fn prepare_native_runtime_state(
    home: &std::path::Path,
    config: &CodeyConfig,
) -> Result<PreparedProviderState> {
    let runtime_config_home = home.to_path_buf();
    let fast_context_tools = config.fast_context_tools;
    let subagent_optimization = config.subagent_optimization;
    let native_subagent_config = native_subagent_runtime_config(config);
    let subagent_model = native_subagent_config.subagent_model;
    let subagent_reasoning_effort = native_subagent_config.subagent_reasoning_effort;
    let subagent_roles = native_subagent_config.subagent_roles;
    let applied = tokio::task::spawn_blocking(move || {
        apply_runtime_router_config(
            &runtime_config_home,
            RuntimeRouterConfigOptions {
                local_router: None,
                use_official_catalog: false,
                default_model: None,
                fast_context_tools,
                subagent_optimization,
                subagent_model: &subagent_model,
                subagent_reasoning_effort: &subagent_reasoning_effort,
                subagent_roles: Some(&subagent_roles),
            },
        )
    })
    .await
    .map_err(|error| {
        anyhow::Error::new(error).context("应用原生 Provider 运行配置任务异常退出")
    })??;
    let mut runtime_config = config.clone();
    runtime_config.fast_context_tools = applied.fast_context_tools_active;
    Ok(PreparedProviderState {
        runtime_config,
        runtime_config_overrides: applied.runtime_config_overrides,
    })
}

async fn prepare_startup_patches(
    home: &std::path::Path,
    config: &CodeyConfig,
) -> Result<StartupPatchState> {
    let slim_codex_pet = config.slim_codex_pet;
    let pet_result = configure_startup_pet(home, slim_codex_pet).await;
    let debug_port = codey_runtime_core::ports::select_packaged_codex_debug_port(9229);
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
                config.local_router_enabled,
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
                config.local_router_enabled,
                anyhow::Error::new(error).context("Codex 宠物精简设置任务异常退出"),
            )
            .await);
        }
    };
    Ok(StartupPatchState { debug_port })
}

async fn spawn_and_inject_runtime(
    home: &std::path::Path,
    config: &CodeyConfig,
    handler: &codey_runtime_core::bridge::BridgeHandler,
    injection_scripts: &cdp::PreparedInjectionScripts,
    storage: StartupStorageState,
    patch: &StartupPatchState,
    runtime_config_overrides: &[String],
) -> Result<SpawnedRenderer> {
    let mut spawned = match spawn_codex(
        &storage.app_dir,
        patch.debug_port,
        config.slim_codex_pet,
        config.subagent_optimization,
        config.gpu_launch_mode,
        runtime_config_overrides,
    )
    .await
    {
        Ok(spawned) => spawned,
        Err(error) => {
            return Err(restore_runtime_config_after_error(
                home,
                config.local_router_enabled,
                error,
            )
            .await);
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
        &spawned,
        &child,
        RuntimeConfigRestoreContext {
            home,
            local_router_enabled: config.local_router_enabled,
        },
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

    pub fn sync_local_router_routes(&self, config: &CodeyConfig) {
        if let Some(local_router) = self.local_router.as_ref() {
            local_router.update_config(config);
        }
    }

    pub(crate) async fn reconfigure_request_log(
        &self,
        config: &RouteRequestLogConfig,
    ) -> Result<Option<RouteRequestLogReconfigure>> {
        let Some(local_router) = self.local_router.as_ref() else {
            return Ok(None);
        };
        local_router.reconfigure_request_log(config).await.map(Some)
    }

    pub(crate) fn local_router_endpoint(&self) -> Option<RuntimeRouterEndpoint> {
        self.local_router.as_ref().map(LocalRouter::endpoint)
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
            && self.applied_config.local_router_enabled == config.local_router_enabled
            && self.applied_config.fast_context_tools == config.fast_context_tools
            && self.applied_config.active_profile() == config.active_profile()
    }

    pub(crate) fn subagent_reconcile_config(&self, config: &CodeyConfig) -> CodeyConfig {
        if self.applied_config.local_router_enabled {
            config.clone()
        } else {
            native_subagent_runtime_config(config)
        }
    }

    pub fn set_crashpad_pending_protection(&self, enabled: bool) {
        self.crashpad_guard_enabled
            .store(enabled, Ordering::Release);
    }

    pub async fn crashpad_pending_protection_active(&self) -> bool {
        if !cfg!(target_os = "macos") || !self.crashpad_guard_enabled.load(Ordering::Acquire) {
            return false;
        }

        self.crashpad_guard_task
            .lock()
            .await
            .as_ref()
            .is_some_and(|task| !task.is_finished())
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
            return self.injection_statuses.read().await.clone();
        }
        *self.injection_statuses.write().await = statuses.clone();
        statuses
    }

    pub async fn start(
        config: &CodeyConfig,
        handler: codey_runtime_core::bridge::BridgeHandler,
        trace_log_write_protection_active: &AtomicBool,
        crashpad_pending_stats: CrashpadPendingStatsHandle,
    ) -> Result<(Self, oneshot::Receiver<()>)> {
        let home = codex_home();
        trace_log_write_protection_active.store(false, Ordering::Release);
        let injection_scripts = cdp::prepare_injection_scripts(
            config.local_router_enabled,
            config.slim_codex_pet,
            config.hide_full_access_warning,
            &config.user_scripts,
        );
        let initial_storage_guards = spawn_initial_storage_guards(home, config);
        let startup_profile = config
            .local_router_enabled
            .then(|| resolve_startup_profile(config))
            .transpose()?;
        // When local routing is enabled, threads created or resumed under
        // Codey persist `codey_router` in
        // rollout headers and the Codex thread index. Codex Desktop resolves
        // that id from disk config; process `-c` overlays do not replace that
        // lookup. Sync records back to the user's persistent provider so
        // threads remain loadable outside Codey. Do not install the ChatGPT
        // resume shim here: that table is ChatGPT-account transport and would
        // send third-party catalog aliases to chatgpt.com for the whole live
        // session. The live loopback table is written after the local router
        // binds, inside apply_runtime_router_config. Native mode skips this
        // routing-specific session rewrite and leaves Codex records untouched.
        let persistent_session_provider = if config.local_router_enabled {
            Some(resolve_persistent_session_provider(home).await?)
        } else {
            None
        };
        let session_provider_sync_target = persistent_session_provider.as_deref();
        let storage = prepare_startup_storage(
            home,
            config,
            session_provider_sync_target,
            initial_storage_guards,
            trace_log_write_protection_active,
            &crashpad_pending_stats,
        )
        .await?;
        let local_router = if config.local_router_enabled {
            Some(LocalRouter::start(config).await?)
        } else {
            None
        };
        let prepared_provider_state = if let (Some(startup_profile), Some(local_router)) =
            (startup_profile.as_ref(), local_router.as_ref())
        {
            prepare_runtime_provider_state(home, config, startup_profile, local_router).await
        } else {
            prepare_native_runtime_state(home, config).await
        };
        let PreparedProviderState {
            runtime_config,
            runtime_config_overrides,
        } = match prepared_provider_state {
            Ok(state) => state,
            Err(error) => {
                return Err(restore_runtime_config_after_error(
                    home,
                    config.local_router_enabled,
                    error,
                )
                .await);
            }
        };
        let patch = prepare_startup_patches(home, config).await?;
        let SpawnedRenderer {
            app_dir,
            spawned,
            child,
            maintenance,
            injected_target,
        } = spawn_and_inject_runtime(
            home,
            config,
            &handler,
            &injection_scripts,
            storage,
            &patch,
            &runtime_config_overrides,
        )
        .await?;
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
                exit_watchdog_shutdown: Mutex::new(Some(exit_watchdog_shutdown)),
                exit_watchdog_task: Mutex::new(Some(exit_watchdog_task)),
                crashpad_guard_enabled,
                crashpad_guard_shutdown: Mutex::new(Some(crashpad_guard_shutdown)),
                crashpad_guard_task: Mutex::new(Some(crashpad_guard_task)),
                local_router,
            },
            codex_exit,
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
        let config_restore = restore_runtime_config_for_router_mode(
            codex_home(),
            self.applied_config.local_router_enabled,
        )
        .await;
        let local_router_stop = match self.local_router.as_ref() {
            Some(local_router) => local_router.stop().await,
            None => Ok(()),
        };
        if let Err(error) = &local_router_stop {
            error_log::record_failure(
                "cleanup_failed",
                "stop_local_router",
                format!("{error:#}"),
                serde_json::json!({}),
            );
        }
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
        let mut failures = Vec::new();
        if let Err(error) = process_stop {
            failures.push(format!("清理 Codex 遗留进程失败：{error:#}"));
        }
        if let Err(error) = config_restore {
            failures.push(format!("恢复 Codex 配置失败：{error:#}"));
        }
        if let Err(error) = local_router_stop {
            failures.push(format!("关闭本地线路路由失败：{error:#}"));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(failures.join("；"))
        }
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

fn watchdog_should_reinject(consecutive_failures: &mut u8, health: InjectionHealth) -> bool {
    match health {
        InjectionHealth::Healthy | InjectionHealth::Inconclusive => {
            *consecutive_failures = 0;
            false
        }
        InjectionHealth::Unhealthy => {
            *consecutive_failures = consecutive_failures.saturating_add(1);
            *consecutive_failures >= CDP_WATCHDOG_FAILURE_THRESHOLD
        }
        InjectionHealth::TargetUnavailable => {
            *consecutive_failures = 0;
            true
        }
    }
}

fn session_maintenance_summary(
    provider_sync: Option<&ProviderSyncResult>,
    index_cleanup: &Result<SessionIndexCleanupReport>,
) -> SessionMaintenanceSummary {
    let pruned_entries = match index_cleanup {
        Ok(report) => report.pruned_entries,
        Err(_) => 0,
    };
    let has_errors = provider_sync.is_some_and(|provider_sync| {
        provider_sync.status != ProviderSyncStatus::Synced
            || !provider_sync.skipped_locked_rollout_files.is_empty()
    }) || index_cleanup.is_err();
    let status = if has_errors { "error" } else { "ready" };
    SessionMaintenanceSummary {
        status: status.to_string(),
        files_fixed: provider_sync.map_or(0, |provider_sync| provider_sync.changed_session_files),
        sqlite_rows_updated: provider_sync
            .map_or(0, |provider_sync| provider_sync.sqlite_rows_updated),
        ghost_tasks_pruned: pruned_entries,
    }
}

#[cfg(test)]
mod maintenance_status_tests;

pub async fn restore_previous_runtime_state(
    home: &std::path::Path,
    local_router_enabled: bool,
) -> Result<()> {
    restore_runtime_config_for_router_mode(home, local_router_enabled).await
}

pub async fn prepare_persistent_router_resume_shim(home: &std::path::Path) -> Result<()> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || prepare_persistent_router_resume_shim_blocking(&home))
        .await
        .context("写入 codey_router 恢复兼容桩任务异常退出")?
}

fn prepare_persistent_router_resume_shim_blocking(home: &std::path::Path) -> Result<()> {
    let result = prepare_codex_router_resume_shim(home)
        .map(|_| ())
        .context("写入 codey_router 恢复兼容桩失败");
    if let Err(error) = &result {
        error_log::record_failure(
            "patch_failed",
            "prepare_persistent_router_resume_shim",
            format!("{error:#}"),
            serde_json::json!({
                "codexHome": home,
            }),
        );
    }
    result
}

async fn restore_runtime_config_for_router_mode(
    home: &std::path::Path,
    local_router_enabled: bool,
) -> Result<()> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        restore_runtime_config_for_router_mode_blocking(&home, local_router_enabled)
    })
    .await
    .context("恢复 Codey 运行时配置任务异常退出")?
}

fn restore_runtime_config_for_router_mode_blocking(
    home: &std::path::Path,
    local_router_enabled: bool,
) -> Result<()> {
    let result = restore_codex_runtime_config_for_router_mode(home, local_router_enabled)
        .map(|_| ())
        .context("恢复 Codex 配置失败");
    if let Err(error) = &result {
        error_log::record_failure(
            "restore_failed",
            "restore_runtime_config",
            format!("{error:#}"),
            serde_json::json!({
                "codexHome": home,
                "localRouterEnabled": local_router_enabled,
            }),
        );
    }
    result
}

async fn restore_runtime_config_after_error(
    home: &std::path::Path,
    local_router_enabled: bool,
    error: anyhow::Error,
) -> anyhow::Error {
    match restore_runtime_config_for_router_mode(home, local_router_enabled).await {
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
