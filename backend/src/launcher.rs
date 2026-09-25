#[cfg(all(test, unix))]
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use codey_runtime_core::app_paths::resolve_codex_app_dir_with_saved;
use codey_runtime_core::config_manager::ConfigManager;
use codey_runtime_core::launcher::build_codex_command;
use serde::Serialize;
use tokio::process::Child;
#[cfg(not(windows))]
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock, oneshot};

use crate::cdp;
use crate::codex_config::{
    RuntimeRouterConfigOptions, apply_runtime_router_config, codex_home,
    prepare_persistent_router_resume_shim as prepare_codex_router_resume_shim,
    repair_reserved_provider_ids,
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
use crate::route_request_log::{RouteRequestLogClearResult, RouteRequestLogReconfigure};
use crate::session_index_cleanup::{self, SessionIndexCleanupReport};
use crate::subagent_policy;
use crate::trace_log_guard;

mod platform;
mod process;

use platform::*;
#[cfg(windows)]
pub(crate) use platform::{activate_visible_windows_codex_window, windows_process_in_session};
#[cfg(windows)]
pub(crate) use process::windows_cli_wrapper_target;
use process::{
    SpawnedCodex, prepare_codex_for_launch, reap_child_after_cleanup, spawn_codex,
    spawn_codex_exit_watcher,
};
#[cfg(test)]
use process::{codex_runtime_arguments, gpu_launch_arguments};

const CDP_WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
const CDP_WATCHDOG_FAILURE_THRESHOLD: u8 = 2;
// 页面端点对 CDP 命令完全没有回应：页面忙碌不成立，target 很可能已被替换。
// 连续多轮无响应后重新发现 target，而不是一直当作"页面忙"等下去。
const CDP_WATCHDOG_UNRESPONSIVE_THRESHOLD: u8 = 3;
// 页面仍活着，只是桥接往返失败（例如常驻连接已死却还没报错）。
// 保守等待更久以免页面抖动就重建桥接，但必须有终点，
// 否则这类"半死"状态永远不会自愈。
const CDP_WATCHDOG_INCONCLUSIVE_LIMIT: u8 = 6;
// A completely unresponsive endpoint is a stronger signal than a busy page,
// so it must rebuild sooner.
const _: () = assert!(CDP_WATCHDOG_UNRESPONSIVE_THRESHOLD < CDP_WATCHDOG_INCONCLUSIVE_LIMIT);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InjectionHealth {
    Healthy,
    Unhealthy,
    Inconclusive,
    /// 页面端点对探测命令没有任何回应。
    Unresponsive,
    /// 常驻桥接连接已经结束，页面内的调用再也无法送达。
    BridgeClosed,
    TargetUnavailable,
}

impl InjectionHealth {
    fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Inconclusive => "inconclusive",
            Self::Unresponsive => "unresponsive",
            Self::BridgeClosed => "bridge_closed",
            Self::TargetUnavailable => "target_unavailable",
        }
    }
}

/// Each failure shape keeps its own budget. Letting one shape reset another
/// would let a bridge that alternates between failures avoid reinjection
/// forever, which is exactly how a half-dead bridge escapes every trigger.
/// Only a confirmed healthy probe clears the budgets.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct InjectionFailureCounters {
    unhealthy: u8,
    inconclusive: u8,
    unresponsive: u8,
}

impl InjectionFailureCounters {
    fn snapshot(self) -> serde_json::Value {
        serde_json::json!({
            "unhealthy": self.unhealthy,
            "inconclusive": self.inconclusive,
            "unresponsive": self.unresponsive,
        })
    }

    /// Keep every budget one step below its threshold after a failed rebuild so
    /// the next matching failure retries immediately, without turning the
    /// watchdog into a tight rebuild loop.
    fn after_failed_reinjection() -> Self {
        Self {
            unhealthy: CDP_WATCHDOG_FAILURE_THRESHOLD.saturating_sub(1),
            inconclusive: CDP_WATCHDOG_INCONCLUSIVE_LIMIT.saturating_sub(1),
            unresponsive: CDP_WATCHDOG_UNRESPONSIVE_THRESHOLD.saturating_sub(1),
        }
    }
}
pub const CODEX_APP_NOT_FOUND_ERROR: &str = "找不到 Codex 桌面应用";
pub const CODEX_APP_PATH_INVALID_ERROR: &str = "配置的 Codex App 路径无效或指向了 Codex CLI；请选择 Codex 桌面 App 的安装目录，不要选择 codex.exe 命令行程序或第三方 Codex 启动器";
const DISABLE_GPU_ARGUMENT: &str = "--disable-gpu";
const DISABLE_GPU_RASTERIZATION_ARGUMENT: &str = "--disable-gpu-rasterization";
const DISABLE_BACKGROUND_ECOQOS_ARGUMENT: &str = "--disable-features=UseEcoQoSForBackgroundProcess";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceStatus {
    pub session_status: String,
    pub session_files_fixed: usize,
    pub sqlite_rows_updated: usize,
    pub ghost_tasks_pruned: usize,
    pub performance_status: String,
    pub performance_detail: String,
    pub startup_injection_mode: String,
}

struct SessionMaintenanceSummary {
    status: String,
    files_fixed: usize,
    sqlite_rows_updated: usize,
    ghost_tasks_pruned: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeModelConfig {
    routes: Vec<(String, String, bool, bool, bool)>,
    selected_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    model_reasoning_efforts_by_provider: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>,
    >,
    model_context_by_provider: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, crate::config::ModelContextConfig>,
    >,
    manual_third_party_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    declared_official_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    upstream_models_by_provider: std::collections::BTreeMap<String, Vec<String>>,
    default_model: String,
}

impl RuntimeModelConfig {
    /// Equivalent to `*self == Self::from_config(config)` without cloning the
    /// six model maps; the runtime-status poll calls this on every request.
    pub fn matches(&self, config: &CodeyConfig) -> bool {
        self.routes.len() == config.profiles.len()
            && self
                .routes
                .iter()
                .zip(&config.profiles)
                .all(|(route, profile)| {
                    route.0 == profile.provider_id()
                        && route.1 == profile.name
                        && route.2 == profile.enabled
                        && route.3 == profile.official_account
                        && route.4 == profile.supports_auto_review
                })
            && self.selected_models_by_provider == config.selected_models_by_provider
            && self.model_context_by_provider == config.model_context_by_provider
            && self.model_reasoning_efforts_by_provider
                == config.model_reasoning_efforts_by_provider
            && self.manual_third_party_models_by_provider
                == config.manual_third_party_models_by_provider
            && self.declared_official_models_by_provider
                == config.declared_official_models_by_provider
            && self.upstream_models_by_provider == config.upstream_models_by_provider
            && self.default_model == config.default_model
    }

    pub fn from_config(config: &CodeyConfig) -> Self {
        Self {
            routes: config
                .profiles
                .iter()
                .map(|profile| {
                    (
                        profile.provider_id().to_string(),
                        profile.name.clone(),
                        profile.enabled,
                        profile.official_account,
                        profile.supports_auto_review,
                    )
                })
                .collect(),
            selected_models_by_provider: config.selected_models_by_provider.clone(),
            model_context_by_provider: config.model_context_by_provider.clone(),
            model_reasoning_efforts_by_provider: config.model_reasoning_efforts_by_provider.clone(),
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

struct AppliedModelConfig {
    catalog: CodeyConfig,
    snapshot: Arc<RuntimeModelConfig>,
}

impl AppliedModelConfig {
    fn new(config: CodeyConfig) -> Self {
        let snapshot = Arc::new(RuntimeModelConfig::from_config(&config));
        Self {
            catalog: config,
            snapshot,
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
    pub fn matches(&self, config: &CodeyConfig) -> bool {
        self.model == config.subagent_model
            && self.reasoning_effort == config.subagent_reasoning_effort
            && self.roles == config.subagent_roles
    }

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
    applied_model_config: RwLock<AppliedModelConfig>,
    applied_subagent_config: RwLock<Arc<RuntimeSubagentConfig>>,
    subagent_route_catalog_installed: bool,
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

fn validate_router_provider(home: &std::path::Path) -> Result<()> {
    let config_path = home.join("config.toml");
    let snapshot = ConfigManager::new(&config_path)
        .load()
        .context("读取 Codex 持久配置失败")?;
    let document = snapshot.document();
    if user_owned_router_provider_occupies_id(document) {
        // Validate before startup maintenance. Codey-owned resume shims are
        // compatible with the live loopback table installed later.
        anyhow::bail!(
            "Codex config.toml 已占用 Codey 内部 Provider ID「{}」；请先重命名该自定义 Provider",
            ROUTER_PROVIDER_ID
        );
    }
    Ok(())
}

async fn validate_startup_router_provider(home: &std::path::Path) -> Result<()> {
    let provider_home = home.to_path_buf();
    tokio::task::spawn_blocking(move || validate_router_provider(&provider_home))
        .await
        .map_err(|error| {
            let error = anyhow::Error::new(error).context("校验启动 Provider 配置任务异常退出");
            error_log::record_failure(
                "patch_failed",
                "validate_router_provider",
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
                "validate_router_provider",
                format!("{error:#}"),
                serde_json::json!({
                    "codexHome": home,
                }),
            );
            error
        })
}

/// Codex 内置 `openai` 等 Provider，同名的自定义表会让它直接拒绝加载配置并
/// 退出。第三方中转教程常把自定义端点写成 `[model_providers.openai]`，所以
/// 启动前先改名；修复本身失败只记录诊断信息，不影响启动流程。
async fn repair_startup_reserved_providers(home: &std::path::Path) {
    let provider_home = home.to_path_buf();
    let result =
        tokio::task::spawn_blocking(move || repair_reserved_provider_ids(&provider_home)).await;
    let renames = match result {
        Ok(Ok(renames)) => renames,
        Ok(Err(error)) => {
            record_reserved_provider_repair_failure(home, format!("{error:#}"), false);
            return;
        }
        Err(error) => {
            record_reserved_provider_repair_failure(home, format!("{error}"), true);
            return;
        }
    };
    if renames.is_empty() {
        return;
    }
    let summary = renames
        .iter()
        .map(|rename| format!("{} -> {}", rename.from, rename.to))
        .collect::<Vec<_>>()
        .join("; ");
    error_log::record_failure_with_metadata(
        "config_repaired",
        "repair_reserved_provider_ids",
        format!("已将占用内置 Provider ID 的自定义配置改名：{summary}"),
        error_log::FailureMetadata {
            stage: Some("startup.config_repair".to_string()),
            recoverable: Some(true),
        },
        serde_json::json!({
            "codexHome": home,
            "renames": renames
                .iter()
                .map(|rename| serde_json::json!({
                    "from": rename.from,
                    "to": rename.to,
                }))
                .collect::<Vec<_>>(),
        }),
    );
}

fn record_reserved_provider_repair_failure(
    home: &std::path::Path,
    error: String,
    task_join_failed: bool,
) {
    error_log::record_failure_with_metadata(
        "patch_failed",
        "repair_reserved_provider_ids",
        error,
        error_log::FailureMetadata {
            stage: Some("startup.config_repair".to_string()),
            recoverable: Some(true),
        },
        serde_json::json!({
            "codexHome": home,
            "taskJoinFailed": task_join_failed,
        }),
    );
}

async fn run_startup_session_maintenance(
    home: &std::path::Path,
) -> Result<SessionMaintenanceSummary> {
    let maintenance_home = home.to_path_buf();
    let maintenance_result = tokio::task::spawn_blocking(move || {
        let stale_lock_recovery = maintenance_lock::recover_stale_locks(&maintenance_home);
        // A loaded Codex thread may have flushed a deleted turn after the live
        // request completed. Reapply durable tombstones after the old process
        // is stopped and before the new process can hydrate that stale data.
        let message_delete_replay = message_delete::reapply_persisted_deletions(&maintenance_home);
        // `session_index.jsonl` is also cleaned before spawn, while its
        // source snapshot is stable. The original file is backed up.
        let index_cleanup = session_index_cleanup::cleanup(&maintenance_home);
        (stale_lock_recovery, message_delete_replay, index_cleanup)
    })
    .await;
    let (stale_lock_recovery, message_delete_replay, index_cleanup) = match maintenance_result {
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
    Ok(session_maintenance_summary(&index_cleanup))
}

async fn resolve_configured_codex_app_dir(config: &CodeyConfig) -> Result<PathBuf> {
    let configured_app_path = config.codex_app_path.trim();
    let configured_app_path_is_empty = configured_app_path.is_empty();
    let configured_app_path =
        (!configured_app_path_is_empty).then(|| PathBuf::from(configured_app_path));
    tokio::task::spawn_blocking(move || {
        let app_dir = resolve_codex_app_dir_with_saved(configured_app_path.as_deref(), None);
        if let Some(app_dir) = app_dir.as_deref() {
            error_log::refresh_codex_app_version(Some(app_dir), None);
        }
        app_dir
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
    config: &CodeyConfig,
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
        return config.runtime_catalog_id_for_target(target);
    }
    let official_route = targets
        .iter()
        .any(|target| target.official && target.provider_id == route_provider);
    if config.uses_builtin_official_model_catalog() || official_route {
        requested.to_string()
    } else {
        local_router::model_alias(route_provider, requested)
    }
}

fn should_install_codey_model_catalog(
    official_only: bool,
    catalog_available: bool,
    custom_context: bool,
) -> bool {
    (!official_only || custom_context) && catalog_available
}

fn router_subagent_runtime_config(
    config: &CodeyConfig,
    route_catalog_installed: bool,
) -> Result<CodeyConfig> {
    let mut runtime = config.clone();
    if !config.subagent_optimization {
        return Ok(runtime);
    }
    let targets = config.runtime_model_targets();
    let provider = config.current_provider_id().unwrap_or_default();
    for (role, selection) in &mut runtime.subagent_roles {
        if !selection.enabled {
            continue;
        }
        let routed = route_subagent_model(provider, &selection.model, &targets, config);
        selection.model = if route_catalog_installed {
            routed
        } else {
            let model = native_subagent_model(config, &targets, &routed);
            // Without a custom catalog Codex validates native slugs before the
            // router sees the request. Only strip a route when raw-id routing
            // is unambiguous, including inherited thread route metadata.
            let mut matching = targets
                .iter()
                .filter(|target| model_id::equal(&target.upstream_model, &model));
            anyhow::ensure!(
                matching.next().is_some() && matching.next().is_none(),
                "子代理角色 {role} 的模型 {routed} 无法在内置模型目录模式下安全派发：模型线路不存在或存在同名线路；请补齐 Codex 模型缓存并重启，或只启用一条提供该模型的线路"
            );
            // 路由是否唯一与模型品牌无关。Codey 的官方展示名单不能
            // 代表当前 Codex 或自定义 Provider 实际支持的模型范围。
            model
        };
    }
    if let Some(default) = runtime
        .subagent_roles
        .get(crate::config::SUBAGENT_ROLE_DEFAULT)
    {
        runtime.subagent_model.clone_from(&default.model);
    }
    Ok(runtime)
}

fn validated_router_subagent_runtime_config(
    config: &CodeyConfig,
    route_catalog_installed: bool,
    home: &std::path::Path,
) -> Result<CodeyConfig> {
    if !config.subagent_optimization {
        return Ok(config.clone());
    }
    let catalog_path =
        crate::codex_config::runtime_model_catalog_path(home, route_catalog_installed)?;
    let runtime = router_subagent_runtime_config(config, catalog_path.is_some())?;
    if let Some(path) = catalog_path {
        model_catalog::validate_runtime_subagent_models(&path, &runtime.subagent_roles)?;
    }
    Ok(runtime)
}

#[test]
fn model_context_explicit_official_budget_requires_generated_catalog() {
    assert!(!should_install_codey_model_catalog(true, true, false));
    assert!(should_install_codey_model_catalog(true, true, true));
    assert!(!should_install_codey_model_catalog(true, false, true));
    assert!(should_install_codey_model_catalog(false, true, false));
}

fn runtime_default_model(
    config: &CodeyConfig,
    codey_catalog_installed: bool,
    model_state: &model_catalog::ModelSelectionState,
) -> Option<String> {
    let model = if codey_catalog_installed {
        config
            .effective_runtime_default_target()
            .map(|target| config.runtime_catalog_id_for_target(&target))
            .or_else(|| {
                config
                    .profiles
                    .iter()
                    .any(|profile| profile.enabled)
                    .then(|| config.default_model().map(str::to_string))
                    .flatten()
            })
            .unwrap_or_else(|| model_state.default_model.clone())
    } else {
        // The built-in Codex catalog only contains native OpenAI model ids.
        model_state.default_model.clone()
    };
    let model = model.trim();
    (!model.is_empty()).then(|| model.to_string())
}

async fn prepare_startup_model_catalog(
    config: &CodeyConfig,
    current_profile: &ProviderProfile,
    home: &std::path::Path,
    codex_app_dir: &std::path::Path,
) -> Result<StartupModelCatalog> {
    let catalog_home = home.to_path_buf();
    // The snapshot has to come from the install this launch uses, so the
    // resolved app directory is forwarded instead of re-probing the default.
    let catalog_codex_app_path = codex_app_dir.to_string_lossy().into_owned();
    let use_builtin_official_catalog =
        current_profile.enabled && config.uses_builtin_official_model_catalog();
    let official_provider = use_builtin_official_catalog
        && current_profile.official_account
        && config.official_account_available_this_launch;
    let (runtime_upstream_models, runtime_selected_models) = config.runtime_catalog_models();
    let runtime_websocket_models = config.runtime_websocket_model_aliases();
    let runtime_native_web_search_models = config.runtime_native_web_search_model_aliases();
    let runtime_image_detail_original_models = config.runtime_image_detail_original_model_aliases();
    let runtime_model_reasoning_efforts = config.runtime_model_reasoning_efforts();
    let runtime_model_contexts = config.runtime_model_contexts();
    let custom_context = !runtime_model_contexts.is_empty();
    let refresh_official_provider =
        config.official_account_available_this_launch && use_builtin_official_catalog;
    let refresh_upstream_models =
        (!use_builtin_official_catalog).then_some(runtime_upstream_models);
    let current_provider_id = current_profile.provider_id();
    let upstream_models = current_profile
        .enabled
        .then(|| {
            config
                .upstream_models_by_provider
                .get(current_provider_id)
                .cloned()
        })
        .flatten();
    let selected_models = if !current_profile.enabled {
        Vec::new()
    } else if official_provider {
        config
            .selected_models_by_provider
            .get(current_provider_id)
            .cloned()
            .unwrap_or_default()
    } else {
        config.enabled_route_models(current_provider_id)
    };
    let manual_models = current_profile
        .enabled
        .then(|| {
            config
                .manual_third_party_models_by_provider
                .get(current_provider_id)
                .cloned()
        })
        .flatten()
        .unwrap_or_default();
    let requested_default_model = current_profile
        .enabled
        .then(|| config.default_model_for_profile(current_profile))
        .flatten();
    let (refresh_result, cached_catalog_result, selection_result) =
        tokio::task::spawn_blocking(move || {
            let refresh = model_catalog::refresh_for_provider_with_contexts(
                &catalog_home,
                refresh_official_provider,
                refresh_upstream_models.as_deref(),
                &runtime_selected_models,
                model_catalog::CapabilityLists {
                    websocket_models: Some(&runtime_websocket_models),
                    native_web_search_models: Some(&runtime_native_web_search_models),
                    image_detail_original_models: Some(&runtime_image_detail_original_models),
                },
                model_catalog::CatalogOverrides {
                    contexts: &runtime_model_contexts,
                    reasoning_efforts: &runtime_model_reasoning_efforts,
                },
                &catalog_codex_app_path,
            );
            let cached_catalog = if refresh.is_err() {
                model_catalog::prepare_cached_catalog_for_current_capabilities(
                    &catalog_home,
                    &runtime_native_web_search_models,
                    &runtime_image_detail_original_models,
                )
                .and_then(|available| {
                    if available {
                        model_catalog::apply_catalog_overrides(
                            &catalog_home,
                            model_catalog::CatalogOverrides {
                                contexts: &runtime_model_contexts,
                                reasoning_efforts: &runtime_model_reasoning_efforts,
                            },
                        )?;
                    }
                    Ok(available)
                })
            } else {
                Ok(false)
            };
            let selection = model_catalog::selection_state_with_manual_models(
                &catalog_home,
                official_provider,
                upstream_models.as_deref(),
                &selected_models,
                &manual_models,
                Some(&runtime_model_reasoning_efforts),
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
        // Codex rejects an empty catalog even when writing it succeeded.
        Ok(count) => count > 0,
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
    // Official-only launches inherit Codex metadata unless the user explicitly
    // configured a budget, in which case the generated catalog must be used.
    if custom_context && !catalog_available_for_runtime {
        anyhow::bail!(model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE);
    }
    let use_official_catalog = should_install_codey_model_catalog(
        use_builtin_official_catalog,
        catalog_available_for_runtime,
        custom_context,
    );
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
    let stream_max_retries = config.stream_max_retries;
    let runtime_default_model = runtime_default_model(config, use_official_catalog, &model_state);
    let fast_context_tools = config.fast_context_tools;
    let mut runtime_subagent_config = config.clone();
    runtime_subagent_config.active_profile_id = current_profile.id.clone();
    subagent_policy::reconcile_with_model_state(&mut runtime_subagent_config, Some(&model_state));
    let runtime_roles_config = startup_router_subagent_runtime_config(
        &mut runtime_subagent_config,
        use_official_catalog,
        home,
    );
    let subagent_optimization = runtime_subagent_config.subagent_optimization;
    let subagent_model = runtime_roles_config.subagent_model.clone();
    let subagent_reasoning_effort = runtime_subagent_config.subagent_reasoning_effort.clone();
    let subagent_roles = runtime_roles_config.subagent_roles.clone();
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
                stream_max_retries,
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

fn startup_router_subagent_runtime_config(
    runtime_config: &mut CodeyConfig,
    route_catalog_installed: bool,
    home: &std::path::Path,
) -> CodeyConfig {
    match validated_router_subagent_runtime_config(runtime_config, route_catalog_installed, home) {
        Ok(roles) => roles,
        Err(error) => {
            runtime_config.subagent_optimization = false;
            let message = format!(
                "本次启动已停用子代理增强，Codey 将继续启动；已保留原设置，修复模型线路后重启可恢复。原因：{error:#}"
            );
            error_log::record_failure_with_metadata(
                "subagent_optimization_unavailable",
                "prepare_startup_subagent_config",
                &message,
                error_log::FailureMetadata {
                    stage: Some("startup.subagent_config".to_string()),
                    recoverable: Some(true),
                },
                serde_json::json!({
                    "fallback": "disable_subagent_optimization_for_current_launch",
                    "routeCatalogInstalled": route_catalog_installed,
                }),
            );
            eprintln!("{message}");
            runtime_config.clone()
        }
    }
}

async fn await_initial_storage_guards(
    initial_trace_guard: tokio::task::JoinHandle<Result<trace_log_guard::TraceLogGuardReport>>,
    disable_trace_log_writes: bool,
    trace_log_write_protection_active: &AtomicBool,
    initial_crashpad_guard: tokio::task::JoinHandle<crashpad_pending_guard::CrashpadGuardRun>,
    protect_crashpad_pending: bool,
    crashpad_pending_stats: &CrashpadPendingStatsHandle,
) -> Result<()> {
    let (trace_result, crashpad_result) = tokio::join!(initial_trace_guard, initial_crashpad_guard);
    let trace_result = match trace_result {
        Ok(Ok(report)) => {
            trace_log_write_protection_active.store(
                report.protection_active(disable_trace_log_writes),
                Ordering::Release,
            );
            Ok(())
        }
        Ok(Err(error)) => {
            error_log::record_failure(
                "patch_failed",
                "configure_trace_log_guard",
                format!("{error:#}"),
                serde_json::json!({
                    "disabled": disable_trace_log_writes,
                }),
            );
            Err(error)
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
            Err(error)
        }
    };

    match crashpad_result {
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
    trace_result
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
    let failure = match cdp::retry_inject_with_scripts(
        debug_port,
        handler,
        injection_scripts,
        cdp::InjectionRetrySource::Startup,
    )
    .await
    {
        Ok(target) => return Ok(target),
        Err(failure) => failure,
    };
    let error_message = format!("{failure:#}");
    let failure_metadata = error_log::FailureMetadata {
        stage: Some("startup.renderer_injection".to_string()),
        recoverable: Some(false),
    };
    let mut error = failure.into_error();
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
        error = anyhow::anyhow!(
            "{error:#}；Codex 注入失败后的进程清理失败，请退出残留 Codex 后重试：{stop_error:#}"
        );
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
        let mut failure_counters = InjectionFailureCounters::default();
        let mut reported_health: Option<InjectionHealth> = None;
        'watchdog: loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                _ = interval.tick() => {}
            }
            // A finished message pump means the in-page bridge can no longer
            // reach the backend at all. Probing again cannot change that, so
            // rebuild immediately instead of waiting for the next verdict.
            let health = if target.pump_finished() {
                InjectionHealth::BridgeClosed
            } else {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break 'watchdog,
                    result = cdp::is_target_healthy(target.websocket_url()) => {
                        match result {
                            Ok(cdp::TargetHealth::Healthy) => InjectionHealth::Healthy,
                            Ok(cdp::TargetHealth::Unhealthy) => InjectionHealth::Unhealthy,
                            Ok(cdp::TargetHealth::Unresponsive) => InjectionHealth::Unresponsive,
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
                }
            };
            // Record every transition once so a stuck bridge leaves a trace in
            // the runtime log. Probing is periodic, so steady states stay quiet.
            if reported_health != Some(health) {
                let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                    "bridge.health_changed",
                    serde_json::json!({
                        "health": health.as_str(),
                        "previous": reported_health
                            .map_or("startup", InjectionHealth::as_str),
                        "websocketUrl": target.websocket_url(),
                        "counters": failure_counters.snapshot(),
                    }),
                );
                reported_health = Some(health);
            }
            if !watchdog_should_reinject(&mut failure_counters, health) {
                continue;
            }
            let reinjection = tokio::select! {
                biased;
                _ = &mut shutdown_rx => break 'watchdog,
                result = cdp::retry_inject_with_scripts(
                    debug_port,
                    handler.clone(),
                    &watchdog_scripts,
                    cdp::InjectionRetrySource::Watchdog,
                ) => result,
            };
            match reinjection {
                Ok(reinjected) => {
                    let next_statuses = reinjected.injection_statuses();
                    let next_websocket_url = reinjected.websocket_url_arc();
                    let previous = std::mem::replace(&mut target, reinjected);
                    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                        "bridge.reinjected",
                        serde_json::json!({
                            "reason": health.as_str(),
                            "previousWebsocketUrl": previous.websocket_url(),
                            "websocketUrl": next_websocket_url.as_ref(),
                        }),
                    );
                    *watchdog_statuses.write().await = next_statuses;
                    *watchdog_websocket_url.write().await = next_websocket_url;
                    previous.close().await;
                    failure_counters = InjectionFailureCounters::default();
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
                    failure_counters = InjectionFailureCounters::after_failed_reinjection();
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

/// Accumulates per-stage durations for `CodeyRuntime::start` and writes one
/// `launcher.startup_timings` diagnostic record with the total.
#[derive(Default)]
struct StartupStageTimings {
    started: Option<Instant>,
    previous: Option<Instant>,
    stages: Vec<(&'static str, u64)>,
}

impl StartupStageTimings {
    fn mark(&mut self, stage: &'static str) {
        let now = Instant::now();
        let started = *self.started.get_or_insert(now);
        let previous = self.previous.replace(now).unwrap_or(started);
        self.stages
            .push((stage, now.duration_since(previous).as_millis() as u64));
    }

    fn report(&self) {
        let mut detail = serde_json::Map::new();
        for (stage, millis) in &self.stages {
            detail.insert((*stage).to_string(), serde_json::json!(millis));
        }
        if let Some(started) = self.started {
            detail.insert(
                "totalMs".to_string(),
                serde_json::json!(started.elapsed().as_millis() as u64),
            );
        }
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.startup_timings",
            serde_json::Value::Object(detail),
        );
    }
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
        .or_else(|| {
            config
                .profiles
                .iter()
                .find(|profile| profile.enabled)
                .cloned()
        })
        .or_else(|| config.active_profile())
        .ok_or_else(|| anyhow::anyhow!("找不到全局默认模型所属的 Codex 线路"))?;
    if current_profile.enabled {
        if current_profile.official_account && !config.official_route_usable(&current_profile) {
            anyhow::bail!("当前线路需要官方账号登录，但本次 Codex 启动未检测到可用的官方登录态");
        }
        // The empty default placeholder is the first-run / no-route state. It
        // is allowed by profile validation so the console can open; requiring
        // an API URL here would exit before the user can add a route.
        if !config.needs_initial_route_import() {
            current_profile.validate().map_err(anyhow::Error::msg)?;
        }
    }
    Ok(current_profile)
}

async fn prepare_startup_storage(
    home: &std::path::Path,
    config: &CodeyConfig,
    current_profile: Option<&ProviderProfile>,
    guards: InitialStorageGuards,
    trace_log_write_protection_active: &AtomicBool,
    crashpad_pending_stats: &CrashpadPendingStatsHandle,
) -> Result<(StartupStorageState, Option<StartupModelCatalog>)> {
    let preparation = async {
        let app_dir = resolve_configured_codex_app_dir(config).await?;
        // Session repair must never race a live Codex writer. Stopping the old
        // runtime first also gives SQLite and rollout buffers a chance to flush
        // before any permanent maintenance is applied.
        prepare_codex_for_launch(&app_dir).await?;

        // Session repair and catalog use other files. Run them together only
        // after the old Codex writer stops.
        let (session_maintenance, startup_catalog) =
            tokio::join!(run_startup_session_maintenance(home), async {
                match current_profile {
                    Some(profile) => prepare_startup_model_catalog(config, profile, home, &app_dir)
                        .await
                        .map(Some),
                    None => Ok(None),
                }
            });
        Ok::<_, anyhow::Error>((
            StartupStorageState {
                app_dir,
                session_maintenance: session_maintenance?,
            },
            startup_catalog?,
        ))
    };
    let storage_guards = await_initial_storage_guards(
        guards.trace,
        config.disable_trace_log_writes,
        trace_log_write_protection_active,
        guards.crashpad,
        config.protect_crashpad_pending,
        crashpad_pending_stats,
    );
    // A failed preparation must still finish the already-started blocking
    // guards and publish their status before the caller can retry or exit.
    let (preparation, storage_guards) = tokio::join!(preparation, storage_guards);
    let prepared = preparation?;
    storage_guards?;
    Ok(prepared)
}

async fn prepare_runtime_provider_state(
    home: &std::path::Path,
    config: &CodeyConfig,
    current_profile: &ProviderProfile,
    local_router: &LocalRouter,
    startup_catalog: StartupModelCatalog,
) -> Result<PreparedProviderState> {
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

fn reconciled_native_subagent_runtime_config(
    config: &CodeyConfig,
    home: &std::path::Path,
) -> CodeyConfig {
    let mut runtime_config = native_subagent_runtime_config(config);
    if let Ok(model_state) = crate::commands::native_subagent_model_state(&runtime_config, home) {
        subagent_policy::reconcile_with_model_state(&mut runtime_config, Some(&model_state));
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
    let stream_max_retries = config.stream_max_retries;
    let native_subagent_config = config.clone();
    let applied = tokio::task::spawn_blocking(move || {
        let native_subagent_config = reconciled_native_subagent_runtime_config(
            &native_subagent_config,
            &runtime_config_home,
        );
        apply_runtime_router_config(
            &runtime_config_home,
            RuntimeRouterConfigOptions {
                local_router: None,
                use_official_catalog: false,
                default_model: None,
                fast_context_tools,
                subagent_optimization,
                subagent_model: &native_subagent_config.subagent_model,
                subagent_reasoning_effort: &native_subagent_config.subagent_reasoning_effort,
                subagent_roles: Some(&native_subagent_config.subagent_roles),
                stream_max_retries,
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
    // The vendored helper only probes the port on Windows. A busy 9229 on macOS
    // (another Node/Electron debugger) otherwise leaves Chromium without a
    // remote-debugging port and the injection times out after 30 s.
    prepare_startup_patches_with_port_selector(home, config, || {
        codey_runtime_core::ports::try_select_packaged_codex_debug_port_with(
            9229,
            true,
            codey_runtime_core::ports::can_bind_loopback_port,
            codey_runtime_core::ports::try_find_available_loopback_port,
        )
    })
    .await
}

async fn prepare_startup_patches_with_port_selector(
    home: &std::path::Path,
    config: &CodeyConfig,
    select_debug_port: impl FnOnce() -> std::io::Result<u16>,
) -> Result<StartupPatchState> {
    let debug_port = select_debug_port().context("无法为 Codex 分配本地调试端口")?;
    anyhow::ensure!(debug_port != 0, "无法为 Codex 分配有效的本地调试端口");
    let slim_codex_pet = config.slim_codex_pet;
    let pet_result = configure_startup_pet(home, slim_codex_pet).await;
    match pet_result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            error_log::record_failure_with_metadata(
                "patch_failed",
                "configure_codex_pet_slim",
                format!("{error:#}"),
                error_log::FailureMetadata {
                    stage: Some("startup.pet_slim".to_string()),
                    recoverable: Some(true),
                },
                serde_json::json!({
                    "enabled": slim_codex_pet,
                    "fallback": "continue_startup",
                }),
            );
        }
        Err(error) => {
            error_log::record_failure_with_metadata(
                "patch_failed",
                "configure_codex_pet_slim",
                error.to_string(),
                error_log::FailureMetadata {
                    stage: Some("startup.pet_slim".to_string()),
                    recoverable: Some(true),
                },
                serde_json::json!({
                    "enabled": slim_codex_pet,
                    "taskJoinFailed": true,
                    "fallback": "continue_startup",
                }),
            );
        }
    };
    Ok(StartupPatchState { debug_port })
}

async fn spawn_and_inject_runtime(
    home: &std::path::Path,
    config: &CodeyConfig,
    handler: &codey_runtime_core::bridge::BridgeHandler,
    injection_scripts: &cdp::PreparedInjectionScripts,
    mut storage: StartupStorageState,
    patch: &StartupPatchState,
    runtime_config_overrides: &[String],
) -> Result<SpawnedRenderer> {
    let spawn_inject_started = Instant::now();
    let mut spawned = match spawn_codex(
        &mut storage.app_dir,
        patch.debug_port,
        config.slim_codex_pet,
        config.subagent_optimization,
        config.misc_model_catalog_id(),
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
    let codex_spawn_ms = spawn_inject_started.elapsed().as_millis() as u64;
    let maintenance = MaintenanceStatus {
        session_status: storage.session_maintenance.status,
        session_files_fixed: storage.session_maintenance.files_fixed,
        sqlite_rows_updated: storage.session_maintenance.sqlite_rows_updated,
        ghost_tasks_pruned: storage.session_maintenance.ghost_tasks_pruned,
        performance_status: spawned.performance_status.clone(),
        performance_detail: spawned.performance_detail.clone(),
        startup_injection_mode: spawned.startup_injection_mode.clone(),
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
    let inject_renderer_ms = spawn_inject_started.elapsed().as_millis() as u64 - codex_spawn_ms;
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.spawn_inject_timings",
        serde_json::json!({
            "codexSpawnAndCompatibilityMs": codex_spawn_ms,
            "rendererInjectionMs": inject_renderer_ms,
            "totalMs": spawn_inject_started.elapsed().as_millis() as u64,
        }),
    );
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

    pub async fn applied_model_config(&self) -> Arc<RuntimeModelConfig> {
        Arc::clone(&self.applied_model_config.read().await.snapshot)
    }

    pub async fn applied_model_catalog_config(&self) -> CodeyConfig {
        self.applied_model_config.read().await.catalog.clone()
    }

    pub async fn mark_model_config_applied(&self, config: &CodeyConfig) {
        *self.applied_model_config.write().await = AppliedModelConfig::new(config.clone());
    }

    pub(crate) fn validate_subagent_route_hot_reload(&self, config: &CodeyConfig) -> Result<()> {
        if self.applied_config.local_router_enabled
            && self.applied_config.subagent_optimization
            && !self.subagent_route_catalog_installed
        {
            // ponytail: raw-ID roles (including running children) pin the route map
            // until restart; live changes need explicit per-child route identity.
            let routes = |config: &CodeyConfig| {
                config
                    .runtime_model_targets()
                    .into_iter()
                    .map(|target| (target.provider_id, target.upstream_model, target.official))
                    .collect::<std::collections::BTreeSet<_>>()
            };
            anyhow::ensure!(
                routes(&self.applied_config) == routes(config),
                "当前子代理使用内置模型目录，线路或模型变化需重启 Codex 后生效；已保留当前路由和角色配置"
            );
        }
        Ok(())
    }

    pub(crate) fn sync_local_router_routes(
        &self,
        config: &CodeyConfig,
    ) -> Result<Option<local_router::RouterSnapshotSwap>> {
        self.validate_subagent_route_hot_reload(config)?;
        Ok(self
            .local_router
            .as_ref()
            .map(|local_router| local_router.update_config(config)))
    }

    pub(crate) fn revert_local_router_routes(&self, swap: local_router::RouterSnapshotSwap) {
        if let Some(local_router) = self.local_router.as_ref() {
            local_router.revert_config(swap);
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

    pub(crate) async fn clear_request_logs(&self) -> Option<RouteRequestLogClearResult> {
        let local_router = self.local_router.as_ref()?;
        Some(local_router.clear_request_logs().await)
    }

    pub(crate) async fn request_log_health(
        &self,
    ) -> Option<crate::route_request_log::RouteRequestLogHealth> {
        Some(self.local_router.as_ref()?.request_log_health().await)
    }

    pub(crate) fn local_router_endpoint(&self) -> Option<RuntimeRouterEndpoint> {
        self.local_router.as_ref().map(LocalRouter::endpoint)
    }

    pub async fn applied_subagent_config(&self) -> Arc<RuntimeSubagentConfig> {
        Arc::clone(&*self.applied_subagent_config.read().await)
    }

    pub async fn mark_subagent_config_applied(&self, config: &CodeyConfig) {
        *self.applied_subagent_config.write().await =
            Arc::new(RuntimeSubagentConfig::from_config(config));
    }

    pub fn supports_subagent_config_hot_reload(&self, config: &CodeyConfig) -> bool {
        self.applied_config.subagent_optimization
            && config.subagent_optimization
            && self.applied_config.local_router_enabled == config.local_router_enabled
            && self.applied_config.fast_context_tools == config.fast_context_tools
            && self.applied_config.active_profile() == config.active_profile()
    }

    pub(crate) fn subagent_reconcile_config(&self, config: &CodeyConfig) -> Result<CodeyConfig> {
        self.validate_subagent_route_hot_reload(config)?;
        if self.applied_config.local_router_enabled {
            validated_router_subagent_runtime_config(
                config,
                self.subagent_route_catalog_installed,
                codex_home(),
            )
        } else {
            Ok(native_subagent_runtime_config(config))
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
        account_usage_cache: Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCaches>>,
    ) -> Result<(Self, oneshot::Receiver<()>)> {
        if !crate::codex_config::runtime_router_platform_supported() {
            return Err(anyhow::anyhow!(
                crate::codex_config::unsupported_runtime_platform_message()
            ));
        }
        let home = codex_home();
        repair_startup_reserved_providers(home).await;
        trace_log_write_protection_active.store(false, Ordering::Release);
        let injection_scripts = cdp::prepare_injection_scripts(
            config.local_router_enabled,
            config.slim_codex_pet,
            config.hide_full_access_warning,
            &config.user_scripts,
        );
        let startup_profile = config
            .local_router_enabled
            .then(|| resolve_startup_profile(config))
            .transpose()?;
        // Stage timings go to the diagnostic log so a slow launch can be
        // attributed without a debugger; values are milliseconds.
        let mut stage_timings = StartupStageTimings::default();
        // apply_runtime_router_config installs the live loopback table before
        // Codex starts, so saved codey_router tasks need no provider rewrite.
        if config.local_router_enabled {
            validate_startup_router_provider(home).await?;
        }
        stage_timings.mark("validateRouterProviderMs");
        let initial_storage_guards = spawn_initial_storage_guards(home, config);
        let (storage, startup_catalog) = prepare_startup_storage(
            home,
            config,
            startup_profile.as_ref(),
            initial_storage_guards,
            trace_log_write_protection_active,
            &crashpad_pending_stats,
        )
        .await?;
        stage_timings.mark("storageAndCatalogMs");
        let local_router = if config.local_router_enabled {
            Some(LocalRouter::start_with_usage(config, account_usage_cache).await?)
        } else {
            None
        };
        stage_timings.mark("localRouterStartMs");
        let prepared_provider_state =
            if let (Some(startup_profile), Some(local_router), Some(startup_catalog)) = (
                startup_profile.as_ref(),
                local_router.as_ref(),
                startup_catalog,
            ) {
                prepare_runtime_provider_state(
                    home,
                    config,
                    startup_profile,
                    local_router,
                    startup_catalog,
                )
                .await
            } else {
                prepare_native_runtime_state(home, config).await
            };
        let PreparedProviderState {
            runtime_config,
            runtime_config_overrides,
        } = match prepared_provider_state {
            Ok(state) => state,
            Err(error) => {
                stop_local_router_after_failed_start(local_router.as_ref()).await;
                return Err(restore_runtime_config_after_error(
                    home,
                    config.local_router_enabled,
                    error,
                )
                .await);
            }
        };
        stage_timings.mark("providerStateMs");
        let patch = match prepare_startup_patches(home, config).await {
            Ok(patch) => patch,
            Err(error) => {
                stop_local_router_after_failed_start(local_router.as_ref()).await;
                return Err(restore_runtime_config_after_error(
                    home,
                    config.local_router_enabled,
                    error,
                )
                .await);
            }
        };
        stage_timings.mark("startupPatchesMs");
        let SpawnedRenderer {
            app_dir,
            spawned,
            child,
            maintenance,
            injected_target,
        } = match spawn_and_inject_runtime(
            home,
            config,
            &handler,
            &injection_scripts,
            storage,
            &patch,
            &runtime_config_overrides,
        )
        .await
        {
            Ok(spawned) => spawned,
            Err(error) => {
                stop_local_router_after_failed_start(local_router.as_ref()).await;
                return Err(error);
            }
        };
        stage_timings.mark("spawnAndInjectMs");
        stage_timings.report();
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
                applied_model_config: RwLock::new(AppliedModelConfig::new(runtime_config.clone())),
                applied_subagent_config: RwLock::new(Arc::new(RuntimeSubagentConfig::from_config(
                    &runtime_config,
                ))),
                subagent_route_catalog_installed: runtime_config_overrides
                    .iter()
                    .any(|entry| entry.starts_with("model_catalog_json=")),
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
        self.stop_with_cleanup(
            stop_codex_processes(
                &self.codex_app_path,
                self.process_id,
                #[cfg(unix)]
                self.process_group_id,
                #[cfg(target_os = "macos")]
                self.inspector_argument.as_deref(),
            ),
            restore_runtime_config_for_router_mode(
                codex_home(),
                self.applied_config.local_router_enabled,
            ),
        )
        .await
    }

    /// 仅用于主程序最终退出：进程树清理失败后，回收仍由本实例持有的直接子进程。
    /// 此操作不表示后代已全部退出，也不允许提前恢复配置或关闭仍可能被使用的路由。
    pub(crate) async fn reap_owned_child_before_exit(&self) -> Result<()> {
        stop_runtime_watcher(
            &self.exit_watchdog_shutdown,
            &self.exit_watchdog_task,
            "process_watch_failed",
            "stop_codex_exit_watcher_before_final_reap",
            "最终退出前关闭 Codex 退出监听器失败",
        )
        .await;
        process::reap_owned_child_before_exit(&self.child).await
    }

    async fn stop_with_cleanup(
        &self,
        process_stop: impl std::future::Future<Output = Result<()>>,
        config_restore: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        // A failed stop leaves a live Codex using this bridge, router and config.
        // Keep its watchers too, so the retained runtime can be stopped again.
        if let Err(error) = process_stop.await {
            error_log::record_failure(
                "cleanup_failed",
                "stop_codex_processes",
                format!("{error:#}"),
                serde_json::json!({
                    "appPath": self.codex_app_path,
                    "processId": self.process_id,
                }),
            );
            return Err(error.context("清理 Codex 遗留进程失败"));
        }
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
        if let Some(child) = self.child.lock().await.take() {
            reap_child_after_cleanup(child, "reap_child_during_runtime_stop").await;
        }
        config_restore.await.context("恢复 Codex 配置失败")?;
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
        local_router_stop.context("关闭本地线路路由失败")
    }
}

/// 启动失败时路由已经在监听。直接 Drop 会中止 accept 任务，来不及排空
/// 正在处理的请求，也不会停掉请求日志。
async fn stop_local_router_after_failed_start(local_router: Option<&LocalRouter>) {
    let Some(local_router) = local_router else {
        return;
    };
    if let Err(error) = local_router.stop().await {
        error_log::record_failure(
            "cleanup_failed",
            "stop_local_router_after_startup_failure",
            format!("{error:#}"),
            serde_json::json!({}),
        );
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

fn watchdog_should_reinject(
    counters: &mut InjectionFailureCounters,
    health: InjectionHealth,
) -> bool {
    match health {
        InjectionHealth::Healthy => {
            *counters = InjectionFailureCounters::default();
            false
        }
        InjectionHealth::Unhealthy => {
            counters.unhealthy = counters.unhealthy.saturating_add(1);
            counters.unhealthy >= CDP_WATCHDOG_FAILURE_THRESHOLD
        }
        InjectionHealth::Inconclusive => {
            counters.inconclusive = counters.inconclusive.saturating_add(1);
            counters.inconclusive >= CDP_WATCHDOG_INCONCLUSIVE_LIMIT
        }
        InjectionHealth::Unresponsive => {
            counters.unresponsive = counters.unresponsive.saturating_add(1);
            counters.unresponsive >= CDP_WATCHDOG_UNRESPONSIVE_THRESHOLD
        }
        InjectionHealth::BridgeClosed | InjectionHealth::TargetUnavailable => {
            *counters = InjectionFailureCounters::default();
            true
        }
    }
}

fn session_maintenance_summary(
    index_cleanup: &Result<SessionIndexCleanupReport>,
) -> SessionMaintenanceSummary {
    let pruned_entries = match index_cleanup {
        Ok(report) => report.pruned_entries,
        Err(_) => 0,
    };
    let has_errors = index_cleanup.is_err();
    let status = if has_errors { "error" } else { "ready" };
    SessionMaintenanceSummary {
        status: status.to_string(),
        files_fixed: 0,
        sqlite_rows_updated: 0,
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

#[cfg(test)]
mod subagent_model_tests;

#[cfg(all(test, unix))]
mod tests;
