use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{Context, Result, bail};
use codey_runtime_core::config_manager::ConfigManager;
use serde::{Deserialize, Serialize};
use toml_edit::{
    Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value, value,
};

#[cfg(test)]
use crate::codex_config_guidance::PREVIOUS_ROOT_AGENT_COLLABORATION_USAGE_HINT;
use crate::codex_config_guidance::{
    CODEY_FASTCTX_GUIDANCE, CODEY_FASTCTX_GUIDANCE_VERSIONS, NO_WRITABLE_SUBAGENT_GUIDANCE,
    READ_ONLY_AGENT_WRITE_GUARD, ROOT_AGENT_COLLABORATION_USAGE_HINT,
    ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS, ROOT_AGENT_MULTI_AGENT_MODE_HINT,
    SUBAGENT_GUIDANCE, SUBAGENT_GUIDANCE_VERSIONS, append_root_agent_collaboration_usage_hint,
    previous_default_agent_config_without_sandbox, remove_codey_fastctx_guidance,
    remove_subagent_guidance, subagent_source_config,
};
use crate::config::{
    CodeyConfig, SUBAGENT_REASONING_EFFORTS, SUBAGENT_ROLE_DEFAULT, SUBAGENT_ROLE_IDS,
    SUBAGENT_ROLE_VISUAL_WORKER, SUBAGENT_ROLE_WORKER, SubagentRoleConfig, default_config_path,
};
#[cfg(test)]
use crate::config::{DEFAULT_SUBAGENT_MODEL, DEFAULT_SUBAGENT_REASONING_EFFORT};
use crate::fs_util::timestamp_millis;
use crate::local_router::{self, RuntimeRouterEndpoint};

mod fastctx;
mod fs_io;
mod runtime_role_transaction;

use fastctx::{
    apply_fastctx_guidance_to_table, arguments_have_codey_fastctx_marker,
    disable_fast_context_tools, enable_fast_context_tools, fast_context_tools_status_from_document,
    remove_guidance_from_table,
};
#[cfg(test)]
use fastctx::{configured_user_fastctx_server_id, direct_only_tool_namespaces, mcp_server_exists};
use fs_io::{
    atomic_write, create_private_dir_all, read_optional, remove_optional, write_private_file,
};
use runtime_role_transaction::refresh_runtime_subagent_roles_at;

pub const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub(crate) const BUILTIN_OPENAI_PROVIDER_ID: &str = "openai";
const LOCAL_ROUTER_PROVIDER_NAME: &str = "Codey Local Router";
const OPENAI_PROVIDER_NAME: &str = "OpenAI";
const CODEY_FASTCTX_SERVER_ID: &str = "codey_fastctx";
const CODEY_FASTCTX_NAMESPACE: &str = "mcp__codey_fastctx";
const CODEY_FASTCTX_ARG_MARKER: &str = "--codey-fastctx-mcp";
const CODEY_FASTCTX_HOST_TOKEN_LIMIT: i64 = 60_000;
const CODEY_FASTCTX_TOKEN_BUDGET: usize = 54_000;
const CODEY_FASTCTX_GREP_TOKEN_BUDGET: usize = 10_800;
const CODEY_FASTCTX_GLOB_TOKEN_BUDGET: usize = 5_400;
const CODEY_FASTCTX_STARTUP_TIMEOUT_SECONDS: i64 = 120;
const CODEY_FASTCTX_TOOL_TIMEOUT_SECONDS: i64 = 300;
const DEFAULT_SUBAGENT_MAX_CONCURRENCY: i64 = 3;
const PREVIOUS_DEFAULT_SUBAGENT_MAX_CONCURRENCY: i64 = 2;
const APPLIED_AGENTS_MD_FILE: &str = "applied-AGENTS.md";
const APPLIED_DEFAULT_AGENT_FILE: &str = "agents/applied-default.toml";
const APPLIED_HOOKS_JSON_FILE: &str = "applied-hooks.json";
const CODEY_CONSTRAINTS_DIR: &str = "codex-constraints";
const CODEY_ROOT_INSTRUCTIONS_FILE: &str = "root-instructions.md";
const CODEY_FASTCTX_INSTRUCTIONS_FILE: &str = "fastctx-instructions.md";
const CODEY_COLLABORATION_HINT_FILE: &str = "collaboration-hint.md";
const CODEY_SUBAGENT_SOURCE_FILE: &str = "subagent.toml";
const CODEY_RUNTIME_DEFAULT_AGENT_FILE: &str = "runtime/default-agent.toml";
const CODEY_SUBAGENT_SOURCES_DIR: &str = "agents";
const CODEY_RUNTIME_AGENTS_DIR: &str = "runtime/agents";
const CODEY_HOOKS_DESCRIPTION: &str = "Codey runtime routing and coordination hooks.";
const CODEY_RUNTIME_CONFIG_LOCK_FILE: &str = "codex-runtime-config.lock";
const RUNTIME_CONFIG_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_CONFIG_LOCK_RETRY: Duration = Duration::from_millis(10);
const RUNTIME_AGENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeConfigLease {
    backup_dir: PathBuf,
    #[serde(default = "lease_default_true")]
    local_router_applied: bool,
    #[serde(default)]
    fastctx_command: Option<PathBuf>,
    #[serde(default)]
    subagent_optimization_applied: bool,
    #[serde(default)]
    subagent_model: String,
    #[serde(default)]
    subagent_reasoning_effort: String,
    #[serde(default)]
    subagent_roles: BTreeMap<String, SubagentRoleConfig>,
    #[serde(default)]
    runtime_home: PathBuf,
    #[serde(default)]
    runtime_agent_schema_version: u32,
    #[serde(default)]
    runtime_agent_hashes: BTreeMap<String, String>,
    #[serde(default)]
    original_agents_md_exists: bool,
    #[serde(default)]
    original_default_agent_exists: bool,
    #[serde(default)]
    original_agents_dir_exists: bool,
    #[serde(default)]
    isolated_runtime_constraints: bool,
    #[serde(default)]
    independent_prompt_sources: bool,
    #[serde(default)]
    runtime_hooks_applied: bool,
    #[serde(default)]
    original_hooks_file_exists: bool,
}

fn lease_default_true() -> bool {
    true
}

pub fn codex_home() -> &'static Path {
    static CODEX_HOME: OnceLock<PathBuf> = OnceLock::new();
    CODEX_HOME
        .get_or_init(codey_runtime_core::relay_config::default_codex_home_dir)
        .as_path()
}

fn read_codex_config(path: &Path) -> Result<Option<Vec<u8>>> {
    let snapshot = ConfigManager::new(path).load()?;
    Ok(snapshot.exists().then(|| snapshot.raw().to_vec()))
}

fn read_or_create_codex_config(path: &Path) -> Result<Vec<u8>> {
    let manager = ConfigManager::new(path);
    let snapshot = manager.load()?;
    if snapshot.exists() {
        return Ok(snapshot.raw().to_vec());
    }
    let created = manager.replace_text(
        Some(snapshot.revision()),
        "",
        "create empty Codex config.toml for desktop compatibility",
        "codex_config.read_or_create_codex_config",
    )?;
    Ok(created.raw().to_vec())
}

fn codex_config_matches(path: &Path, expected: Option<&[u8]>) -> Result<bool> {
    Ok(read_codex_config(path)?.as_deref() == expected)
}

fn lease_marker_path() -> PathBuf {
    default_config_path().with_file_name("codex-lease.json")
}

struct RuntimeConfigLock {
    file: fs::File,
}

fn runtime_config_lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

impl RuntimeConfigLock {
    fn acquire(marker: &Path) -> Result<Self> {
        Self::acquire_with_timeout(marker, RUNTIME_CONFIG_LOCK_TIMEOUT)
    }

    fn acquire_with_timeout(marker: &Path, timeout: Duration) -> Result<Self> {
        let parent = marker
            .parent()
            .expect("runtime config marker paths must include a file name");
        create_private_dir_all(parent)?;
        let lock_path = parent.join(CODEY_RUNTIME_CONFIG_LOCK_FILE);
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&lock_path).with_context(|| {
            format!("打开 Codey Codex 运行时配置锁失败：{}", lock_path.display())
        })?;
        let started = Instant::now();
        loop {
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Self { file }),
                Err(error) if runtime_config_lock_is_contended(&error) => {
                    let elapsed = started.elapsed();
                    if elapsed >= timeout {
                        bail!(
                            "等待 Codey Codex 运行时配置锁超过 {} 毫秒：{}",
                            timeout.as_millis(),
                            lock_path.display()
                        );
                    }
                    thread::sleep(RUNTIME_CONFIG_LOCK_RETRY.min(timeout.saturating_sub(elapsed)));
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("获取 Codey Codex 运行时配置锁失败：{}", lock_path.display())
                    });
                }
            }
        }
    }
}

impl Drop for RuntimeConfigLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

pub(crate) struct RuntimeRouterConfigOptions<'a> {
    pub local_router: Option<&'a RuntimeRouterEndpoint>,
    pub use_official_catalog: bool,
    pub default_model: Option<&'a str>,
    pub fast_context_tools: bool,
    pub subagent_optimization: bool,
    pub subagent_model: &'a str,
    pub subagent_reasoning_effort: &'a str,
    pub subagent_roles: Option<&'a BTreeMap<String, SubagentRoleConfig>>,
}

#[derive(Debug)]
pub(crate) struct AppliedRuntimeRouterConfig {
    pub runtime_config_overrides: Vec<String>,
    pub fast_context_tools_active: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FastContextToolsStatus {
    pub user_configured: bool,
    pub detection_failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
}

struct RouterApplyOptions<'a> {
    local_router: Option<&'a RuntimeRouterEndpoint>,
    use_official_catalog: bool,
    default_model: Option<&'a str>,
    fastctx_command: Option<&'a Path>,
    subagent_optimization: bool,
    subagent_model: &'a str,
    subagent_reasoning_effort: &'a str,
    subagent_roles: Option<&'a BTreeMap<String, SubagentRoleConfig>>,
    marker: &'a Path,
    backup_root: &'a Path,
}

#[derive(Clone, Debug)]
struct RuntimeAgentRegistration {
    role: String,
    description: String,
    config_file: PathBuf,
    content_sha256: String,
}

#[derive(Clone, Debug)]
struct RuntimeAgentPlan {
    registration: RuntimeAgentRegistration,
    contents: Vec<u8>,
}

pub(crate) fn apply_runtime_router_config(
    home: &Path,
    options: RuntimeRouterConfigOptions<'_>,
) -> Result<AppliedRuntimeRouterConfig> {
    let marker = lease_marker_path();
    let _runtime_config_lock = RuntimeConfigLock::acquire(&marker)?;
    let backup_root = marker.with_file_name("codex-backups");
    let fastctx_command = resolve_fastctx_command(options.fast_context_tools);
    let use_official_catalog = options.use_official_catalog;
    let default_model = options.default_model;
    // The startup patch supplies the runtime gate that makes these generated
    // roles safe. Never install the coupled role/prompt/hook configuration on
    // a platform where that patch is not available.
    let subagent_optimization =
        options.subagent_optimization && cfg!(any(windows, target_os = "macos"));
    if !cfg!(any(windows, target_os = "macos")) {
        bail!(
            "当前平台尚不能把 Codey Provider 配置限定到单次 Codex 进程；为避免修改用户 config.toml，已取消启动"
        );
    }
    // Most runtime values stay command-local `-c` overlays. Codex Desktop still
    // looks up a thread's saved `model_provider` from disk, so persist only the
    // live loopback `codey_router` table after the isolated overlay is ready.
    let applied = apply_isolated_runtime_router_config(
        home,
        RouterApplyOptions {
            local_router: options.local_router,
            use_official_catalog,
            default_model,
            fastctx_command: fastctx_command.as_deref(),
            subagent_optimization,
            subagent_model: options.subagent_model,
            subagent_reasoning_effort: options.subagent_reasoning_effort,
            subagent_roles: options.subagent_roles,
            marker: &marker,
            backup_root: &backup_root,
        },
    )?;
    if let Some(local_router) = options.local_router {
        persist_runtime_router_disk_provider_or_rollback(home, &marker, local_router)?;
    }
    Ok(applied)
}

fn persist_runtime_router_disk_provider_or_rollback(
    home: &Path,
    marker: &Path,
    endpoint: &RuntimeRouterEndpoint,
) -> Result<()> {
    if let Err(error) = prepare_runtime_router_disk_provider_at(home, endpoint) {
        return match restore_runtime_config_at(home, marker, true) {
            Ok(_) => Err(error).context("写入运行时 codey_router 磁盘表失败，已回滚隔离运行配置"),
            Err(rollback_error) => anyhow::bail!(
                "写入运行时 codey_router 磁盘表失败：{error:#}；回滚隔离运行配置也失败：{rollback_error:#}"
            ),
        };
    }
    Ok(())
}

const FASTCTX_SERVER_BINARY: &str = if cfg!(windows) {
    "codey-fastctx.exe"
} else {
    "codey-fastctx"
};

/// FastCtx 以 sidecar 程序随 Codey 一起分发，主程序因此不携带内嵌分词器
/// 常量。启用了 FastCtx 但 sidecar 缺失时降级为本次不注册该工具：损失的是
/// 可选增强，而中止启动会让 Codex 完全用不了；缺失会记入错误日志便于定位
/// 打包问题。
fn resolve_fastctx_command(fast_context_tools: bool) -> Option<PathBuf> {
    if !fast_context_tools {
        return None;
    }
    match fastctx_server_command() {
        Ok(command) => Some(command),
        Err(error) => {
            eprintln!("Codey 本次未启用 FastCtx：{error:#}");
            crate::error_log::record_failure(
                "fastctx_sidecar_missing",
                "resolve_fastctx_command",
                format!("{error:#}"),
                serde_json::json!({}),
            );
            None
        }
    }
}

fn fastctx_server_command() -> Result<PathBuf> {
    let current = std::env::current_exe().context("定位 Codey FastCtx 服务失败")?;
    current
        .parent()
        .map(|dir| dir.join(FASTCTX_SERVER_BINARY))
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            anyhow::anyhow!("未在 Codey 程序目录找到 FastCtx 服务程序 {FASTCTX_SERVER_BINARY}")
        })
}

fn apply_isolated_runtime_router_config(
    home: &Path,
    options: RouterApplyOptions<'_>,
) -> Result<AppliedRuntimeRouterConfig> {
    let RouterApplyOptions {
        local_router,
        use_official_catalog,
        default_model,
        fastctx_command,
        subagent_optimization,
        subagent_model,
        subagent_reasoning_effort,
        subagent_roles,
        marker,
        backup_root,
    } = options;
    fs::create_dir_all(home)?;
    let config_path = home.join("config.toml");
    let hooks_path = home.join("hooks.json");
    let original_config = if local_router.is_some() {
        Some(read_or_create_codex_config(&config_path)?)
    } else {
        read_codex_config(&config_path)?
    };
    let existing = str::from_utf8(original_config.as_deref().unwrap_or_default())
        .context("Codex config.toml 不是 UTF-8")?;
    let persistent = parse_document(existing).context("解析 Codex config.toml 失败")?;
    if local_router.is_some() && user_owned_router_provider_occupies_id(&persistent) {
        anyhow::bail!(
            "Codex config.toml 已占用 Codey 内部 Provider ID「{}」；请先重命名该自定义 Provider",
            local_router::ROUTER_PROVIDER_ID
        );
    }
    // Codex resolves this path from the app-server working directory, which is
    // `/` for the packaged macOS app, rather than from CODEX_HOME.
    let model_catalog_path =
        use_official_catalog.then(|| home.join(crate::model_catalog::relative_path()));
    let effective = patch_config_with_fastctx_mode(
        existing,
        RouterPatchOptions {
            config_path: &config_path,
            model_catalog_path: model_catalog_path.as_deref(),
            default_model,
            fastctx_command,
            subagent_optimization,
            subagent_model,
            subagent_reasoning_effort,
            local_router,
        },
    )?;
    let mut effective_document = parse_document(&effective).context("解析 Codey 运行时约束失败")?;
    if use_official_catalog {
        update_model_catalog_reference(
            &mut effective_document,
            &config_path,
            model_catalog_path.as_deref(),
        );
    }
    let fastctx_namespace = effective_document
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(CODEY_FASTCTX_SERVER_ID))
        .and_then(Item::as_table)
        .is_some_and(fastctx_table_server_is_codey_owned)
        .then_some(CODEY_FASTCTX_NAMESPACE);

    let constraints_dir = marker.with_file_name(CODEY_CONSTRAINTS_DIR);
    create_private_dir_all(&constraints_dir)?;
    let fastctx_instructions = if fastctx_namespace.is_some() {
        Some(read_or_create_constraint_file_with_exact_migration(
            &constraints_dir.join(CODEY_FASTCTX_INSTRUCTIONS_FILE),
            CODEY_FASTCTX_GUIDANCE,
            &CODEY_FASTCTX_GUIDANCE_VERSIONS[1..],
        )?)
    } else {
        None
    };
    let runtime_roles =
        runtime_subagent_roles(subagent_roles, subagent_model, subagent_reasoning_effort);
    let (root_instructions, collaboration_hint, runtime_agents) = if subagent_optimization {
        let root_path = constraints_dir.join(CODEY_ROOT_INSTRUCTIONS_FILE);
        let root_instructions = read_or_create_constraint_file_with_exact_migration(
            &root_path,
            SUBAGENT_GUIDANCE,
            &SUBAGENT_GUIDANCE_VERSIONS[1..],
        )?;
        let root_instructions =
            runtime_root_instructions_for_roles(&root_instructions, &runtime_roles);
        let collaboration_hint = read_or_create_constraint_file_with_exact_migration(
            &constraints_dir.join(CODEY_COLLABORATION_HINT_FILE),
            ROOT_AGENT_COLLABORATION_USAGE_HINT,
            &ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS[1..],
        )?;
        let runtime_agents = prepare_runtime_agent_files(
            &constraints_dir,
            &runtime_roles,
            fastctx_instructions.as_deref(),
        )?;
        (
            Some(root_instructions),
            Some(collaboration_hint),
            runtime_agents,
        )
    } else {
        (None, None, Vec::new())
    };
    apply_isolated_prompt_sources(
        &mut effective_document,
        root_instructions.as_deref(),
        fastctx_instructions.as_deref(),
        collaboration_hint.as_deref(),
    )?;

    let runtime_hooks_enabled = subagent_optimization || fastctx_namespace.is_some();
    let original_hooks = if runtime_hooks_enabled {
        read_optional(&hooks_path)?
    } else {
        None
    };
    let subagent_hook_commands = subagent_optimization
        .then(crate::subagent_gate::hook_commands)
        .transpose()?;
    let combined_hook_commands = (subagent_optimization && fastctx_namespace.is_some())
        .then(|| {
            crate::subagent_gate::hook_commands_for(crate::subagent_gate::COMBINED_HOOK_ARGUMENT)
        })
        .transpose()?;
    let fastctx_hook_commands = (fastctx_namespace.is_some() && !subagent_optimization)
        .then(|| crate::subagent_gate::hook_commands_for(crate::fastctx_route_gate::HOOK_ARGUMENT))
        .transpose()?;
    let (updated_hooks, hook_trust_entries) = if runtime_hooks_enabled {
        let RuntimeHooksFile {
            contents,
            trust_entries,
        } = build_runtime_hooks_file(
            original_hooks.as_deref(),
            &hooks_path,
            subagent_hook_commands.as_ref(),
            fastctx_hook_commands.as_ref(),
            combined_hook_commands.as_ref(),
        )?;
        (Some(contents), trust_entries)
    } else {
        (None, Vec::new())
    };
    let runtime_config_overrides = build_isolated_runtime_overrides(
        &effective_document,
        root_instructions.as_deref(),
        &runtime_agents,
        model_catalog_path.as_deref(),
        fastctx_namespace,
        local_router.map(|_| local_router::ROUTER_PROVIDER_ID),
        &hook_trust_entries,
    )?;

    create_private_dir_all(backup_root)?;
    prune_stale_backup_dirs(backup_root, marker);
    let backup_dir = backup_root.join(format!("{}-{}", timestamp_millis(), std::process::id()));
    create_private_dir_all(&backup_dir)?;
    if let Some(bytes) = original_hooks.as_deref() {
        write_private_file(&backup_dir.join("hooks.json"), bytes)?;
    }
    if let Some(bytes) = updated_hooks.as_deref() {
        write_private_file(&backup_dir.join(APPLIED_HOOKS_JSON_FILE), bytes)?;
    }
    let runtime_agent_hashes = runtime_agent_hashes(&runtime_agents);

    let state = RuntimeConfigLease {
        backup_dir: backup_dir.clone(),
        local_router_applied: local_router.is_some(),
        fastctx_command: fastctx_command.map(Path::to_path_buf),
        subagent_optimization_applied: subagent_optimization,
        subagent_model: subagent_model.to_string(),
        subagent_reasoning_effort: subagent_reasoning_effort.to_string(),
        subagent_roles: runtime_roles,
        runtime_home: home.to_path_buf(),
        runtime_agent_schema_version: if subagent_optimization {
            RUNTIME_AGENT_SCHEMA_VERSION
        } else {
            0
        },
        runtime_agent_hashes,
        original_agents_md_exists: false,
        original_default_agent_exists: false,
        original_agents_dir_exists: home.join("agents").is_dir(),
        isolated_runtime_constraints: true,
        independent_prompt_sources: true,
        runtime_hooks_applied: updated_hooks.is_some(),
        original_hooks_file_exists: original_hooks.is_some(),
    };
    if let Err(error) = write_lease(marker, &state) {
        let _ = fs::remove_dir_all(&backup_dir);
        return Err(error);
    }

    let inputs_unchanged = codex_config_matches(&config_path, original_config.as_deref())?
        && (!runtime_hooks_enabled
            || optional_file_matches(&hooks_path, original_hooks.as_deref())?);
    if !inputs_unchanged {
        discard_runtime_lease(home, marker, &backup_dir).with_context(|| {
            "Codex 配置在 Codey 保存隔离约束快照后发生变化；取消启动时清理租约失败，恢复备份已保留"
        })?;
        bail!("Codex 配置在 Codey 保存隔离约束快照后发生变化；已取消本次启动");
    }

    if let Some(updated_hooks) = updated_hooks.as_deref()
        && let Err(write_error) =
            atomic_write(&hooks_path, updated_hooks).context("写入 Codey 独立 Hook 文件失败")
    {
        match rollback_isolated_runtime_config(home, marker, &state) {
            Ok(()) => {
                let _ = fs::remove_dir_all(&backup_dir);
                return Err(write_error);
            }
            Err(rollback_error) => {
                anyhow::bail!("{write_error:#}；清理隔离运行时租约也失败：{rollback_error:#}");
            }
        }
    }

    let policy_result = if subagent_optimization {
        crate::subagent_gate::commit_runtime_subagent_policy(
            home,
            &state.subagent_roles,
            &state.runtime_agent_hashes,
        )
    } else {
        crate::subagent_gate::clear_runtime_subagent_policy(home)
    };
    if let Err(policy_error) = policy_result {
        match rollback_isolated_runtime_config(home, marker, &state) {
            Ok(()) => {
                let _ = fs::remove_dir_all(&backup_dir);
                return Err(policy_error)
                    .context("提交 Codey 子代理运行时策略失败，已回滚隔离运行配置");
            }
            Err(rollback_error) => {
                anyhow::bail!(
                    "提交 Codey 子代理运行时策略失败：{policy_error:#}；清理隔离运行配置也失败：{rollback_error:#}"
                );
            }
        }
    }

    Ok(AppliedRuntimeRouterConfig {
        runtime_config_overrides,
        fast_context_tools_active: fastctx_command.is_some(),
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn apply_isolated_test_runtime_config(
    home: &Path,
    use_official_catalog: bool,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
    subagent_roles: Option<&BTreeMap<String, SubagentRoleConfig>>,
    marker: &Path,
    backup_root: &Path,
) -> Result<AppliedRuntimeRouterConfig> {
    apply_isolated_runtime_router_config(
        home,
        RouterApplyOptions {
            local_router: Some(test_runtime_router_endpoint()),
            use_official_catalog,
            default_model: None,
            fastctx_command,
            subagent_optimization,
            subagent_model,
            subagent_reasoning_effort,
            subagent_roles,
            marker,
            backup_root,
        },
    )
}

fn read_or_create_constraint_file(path: &Path, default_contents: &str) -> Result<String> {
    if let Some(existing) = read_optional(path)? {
        return String::from_utf8(existing)
            .with_context(|| format!("Codey 约束文件不是 UTF-8：{}", path.display()));
    }
    write_private_file(path, default_contents.as_bytes())?;
    Ok(default_contents.to_string())
}

fn read_or_create_constraint_file_with_exact_migration(
    path: &Path,
    default_contents: &str,
    previous_defaults: &[&str],
) -> Result<String> {
    let existing = read_or_create_constraint_file(path, default_contents)?;
    if previous_defaults.contains(&existing.as_str()) {
        atomic_write(path, default_contents.as_bytes())?;
        return Ok(default_contents.to_string());
    }
    Ok(existing)
}

fn runtime_subagent_roles(
    configured: Option<&BTreeMap<String, SubagentRoleConfig>>,
    legacy_model: &str,
    legacy_reasoning_effort: &str,
) -> BTreeMap<String, SubagentRoleConfig> {
    let fallback = configured
        .and_then(|roles| roles.get(SUBAGENT_ROLE_DEFAULT))
        .cloned()
        .unwrap_or_else(|| SubagentRoleConfig::new(legacy_model, legacy_reasoning_effort));
    SUBAGENT_ROLE_IDS
        .into_iter()
        .filter_map(|role| {
            let selection = configured
                .and_then(|roles| roles.get(role))
                .cloned()
                .unwrap_or_else(|| fallback.clone());
            selection.enabled.then(|| (role.to_string(), selection))
        })
        .collect()
}

fn runtime_root_instructions_for_roles(
    root_instructions: &str,
    roles: &BTreeMap<String, SubagentRoleConfig>,
) -> String {
    let has_writable_role =
        roles.contains_key(SUBAGENT_ROLE_WORKER) || roles.contains_key(SUBAGENT_ROLE_VISUAL_WORKER);
    if has_writable_role {
        root_instructions.to_string()
    } else {
        append_constraint_text(root_instructions, NO_WRITABLE_SUBAGENT_GUIDANCE)
    }
}

fn prepare_runtime_agent_files(
    constraints_dir: &Path,
    roles: &BTreeMap<String, SubagentRoleConfig>,
    fastctx_instructions: Option<&str>,
) -> Result<Vec<RuntimeAgentRegistration>> {
    let plans = plan_runtime_agent_files(constraints_dir, roles, fastctx_instructions)?;
    let mut registrations = Vec::with_capacity(plans.len());
    for plan in plans {
        if let Some(parent) = plan.registration.config_file.parent() {
            create_private_dir_all(parent)?;
        }
        atomic_write(&plan.registration.config_file, &plan.contents)?;
        registrations.push(plan.registration);
    }
    remove_unplanned_runtime_agent_files(constraints_dir, &registrations)?;
    Ok(registrations)
}

fn remove_unplanned_runtime_agent_files(
    constraints_dir: &Path,
    registrations: &[RuntimeAgentRegistration],
) -> Result<()> {
    let active_roles = registrations
        .iter()
        .map(|registration| registration.role.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for role in SUBAGENT_ROLE_IDS {
        if active_roles.contains(role) {
            continue;
        }
        let path = runtime_agent_path(constraints_dir, role);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "清理已停用的 Codey 子代理运行时文件失败：{}",
                        path.display()
                    )
                });
            }
        }
    }
    Ok(())
}

fn plan_runtime_agent_files(
    constraints_dir: &Path,
    roles: &BTreeMap<String, SubagentRoleConfig>,
    fastctx_instructions: Option<&str>,
) -> Result<Vec<RuntimeAgentPlan>> {
    let mut plans = Vec::with_capacity(roles.len());
    for role in SUBAGENT_ROLE_IDS {
        let Some(selection) = roles.get(role) else {
            continue;
        };
        let default_source = subagent_source_config(role)
            .with_context(|| format!("缺少 Codey 子代理约束模板：{role}"))?;
        let source_path = if role == SUBAGENT_ROLE_DEFAULT {
            constraints_dir.join(CODEY_SUBAGENT_SOURCE_FILE)
        } else {
            constraints_dir
                .join(CODEY_SUBAGENT_SOURCES_DIR)
                .join(format!("{role}.toml"))
        };
        if let Some(parent) = source_path.parent() {
            create_private_dir_all(parent)?;
        }
        let source = if role == SUBAGENT_ROLE_DEFAULT {
            let previous_default = previous_default_agent_config_without_sandbox();
            read_or_create_constraint_file_with_exact_migration(
                &source_path,
                default_source,
                &[previous_default.as_str()],
            )?
        } else {
            read_or_create_constraint_file(&source_path, default_source)?
        };
        let runtime_path = runtime_agent_path(constraints_dir, role);
        let (contents, description) =
            render_runtime_agent(&source, role, selection, fastctx_instructions)?;
        let content_sha256 = crate::fs_util::sha256_hex(&contents);
        plans.push(RuntimeAgentPlan {
            registration: RuntimeAgentRegistration {
                role: role.to_string(),
                description,
                config_file: runtime_path,
                content_sha256,
            },
            contents,
        });
    }
    Ok(plans)
}

fn runtime_agent_path(constraints_dir: &Path, role: &str) -> PathBuf {
    if role == SUBAGENT_ROLE_DEFAULT {
        constraints_dir.join(CODEY_RUNTIME_DEFAULT_AGENT_FILE)
    } else {
        constraints_dir
            .join(CODEY_RUNTIME_AGENTS_DIR)
            .join(format!("{role}.toml"))
    }
}

fn render_runtime_agent(
    source: &str,
    role: &str,
    selection: &SubagentRoleConfig,
    fastctx_instructions: Option<&str>,
) -> Result<(Vec<u8>, String)> {
    let mut document = parse_document(source).context("解析 Codey 子代理约束文件失败")?;
    let model = selection.model.trim();
    anyhow::ensure!(!model.is_empty(), "子代理任务类型 {role} 的模型不能为空");
    let reasoning_effort = selection.reasoning_effort.trim().to_ascii_lowercase();
    anyhow::ensure!(
        SUBAGENT_REASONING_EFFORTS.contains(&reasoning_effort.as_str()),
        "子代理任务类型 {role} 的思考深度无效：{reasoning_effort}"
    );
    let description = document
        .get("description")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .with_context(|| format!("Codey 子代理约束文件缺少非空 description：{role}"))?
        .to_string();
    remove_guidance_from_table(
        document.as_table_mut(),
        "developer_instructions",
        remove_codey_fastctx_guidance,
    );
    if let Some(instructions) = fastctx_instructions {
        append_table_constraint_text(
            document.as_table_mut(),
            "developer_instructions",
            instructions,
            "developer_instructions",
        )?;
    }
    if !matches!(role, SUBAGENT_ROLE_WORKER | SUBAGENT_ROLE_VISUAL_WORKER) {
        append_table_constraint_text(
            document.as_table_mut(),
            "developer_instructions",
            READ_ONLY_AGENT_WRITE_GUARD,
            "developer_instructions",
        )?;
    }
    document["name"] = value(role);
    document["model"] = value(model);
    document["model_reasoning_effort"] = value(&reasoning_effort);
    let rendered = document_string(&document)?;
    Ok((rendered.into_bytes(), description))
}

fn runtime_agent_hashes(registrations: &[RuntimeAgentRegistration]) -> BTreeMap<String, String> {
    registrations
        .iter()
        .map(|registration| {
            (
                registration.role.clone(),
                registration.content_sha256.clone(),
            )
        })
        .collect()
}

fn apply_isolated_prompt_sources(
    document: &mut DocumentMut,
    root_instructions: Option<&str>,
    fastctx_instructions: Option<&str>,
    collaboration_hint: Option<&str>,
) -> Result<()> {
    remove_guidance_from_table(
        document.as_table_mut(),
        "developer_instructions",
        remove_codey_fastctx_guidance,
    );
    remove_guidance_from_table(
        document.as_table_mut(),
        "developer_instructions",
        remove_subagent_guidance,
    );
    if let Some(instructions) = fastctx_instructions {
        append_table_constraint_text(
            document.as_table_mut(),
            "developer_instructions",
            instructions,
            "developer_instructions",
        )?;
    }
    if let Some(instructions) = root_instructions {
        append_table_constraint_text(
            document.as_table_mut(),
            "developer_instructions",
            instructions,
            "developer_instructions",
        )?;
    }

    let Some(multi_agent) = document
        .get_mut("features")
        .and_then(Item::as_table_like_mut)
        .and_then(|features| features.get_mut("multi_agent_v2"))
        .and_then(Item::as_table_like_mut)
    else {
        return Ok(());
    };
    remove_guidance_from_table(
        multi_agent,
        "subagent_developer_instructions",
        remove_codey_fastctx_guidance,
    );
    remove_guidance_from_table(
        multi_agent,
        "subagent_developer_instructions",
        remove_subagent_guidance,
    );
    if let Some(instructions) = fastctx_instructions {
        append_table_constraint_text(
            multi_agent,
            "subagent_developer_instructions",
            instructions,
            "features.multi_agent_v2.subagent_developer_instructions",
        )?;
    }
    if let Some(collaboration_hint) = collaboration_hint {
        let existing = multi_agent
            .get("root_agent_usage_hint_text")
            .and_then(Item::as_str)
            .unwrap_or_default();
        let cleaned = ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS
            .iter()
            .fold(existing.to_string(), |current, guidance| {
                remove_constraint_text(&current, guidance)
            });
        multi_agent.insert(
            "root_agent_usage_hint_text",
            value(append_constraint_text(&cleaned, collaboration_hint)),
        );
    }
    Ok(())
}

fn append_table_constraint_text(
    table: &mut dyn toml_edit::TableLike,
    key: &str,
    addition: &str,
    qualified_key: &str,
) -> Result<()> {
    let existing = table
        .get(key)
        .map(|item| {
            item.as_str()
                .ok_or_else(|| anyhow::anyhow!("{qualified_key} 必须是字符串"))
        })
        .transpose()?
        .unwrap_or_default();
    let combined = append_constraint_text(existing, addition);
    if combined.trim().is_empty() {
        table.remove(key);
    } else {
        table.insert(key, value(combined));
    }
    Ok(())
}

fn remove_constraint_text(existing: &str, constraint: &str) -> String {
    let constraint = constraint.trim();
    if constraint.is_empty() {
        return existing.to_string();
    }
    let mut current = existing.to_string();
    while let Some(index) = current.find(constraint) {
        let mut start = index;
        if current[..start].ends_with("\n\n") {
            start -= 2;
        }
        let mut end = index + constraint.len();
        if current[end..].starts_with("\n\n") {
            end += 2;
        }
        current.replace_range(start..end, "");
    }
    current.trim().to_string()
}

fn optional_file_matches(path: &Path, expected: Option<&[u8]>) -> Result<bool> {
    Ok(read_optional(path)?.as_deref() == expected)
}

fn discard_runtime_lease(home: &Path, marker: &Path, backup_dir: &Path) -> Result<()> {
    // A cancelled startup must not leave either half of the runtime policy
    // journal behind. Clear it before dropping the lease so a cleanup failure
    // preserves the recovery metadata for the next startup.
    crate::subagent_gate::clear_runtime_subagent_policy(home)?;
    remove_optional(marker)?;
    let _ = fs::remove_dir_all(backup_dir);
    Ok(())
}

fn rollback_isolated_runtime_config(
    home: &Path,
    marker: &Path,
    state: &RuntimeConfigLease,
) -> Result<()> {
    restore_runtime_hooks_file(home, state)?;
    crate::subagent_gate::clear_runtime_subagent_policy(home)?;
    remove_optional(marker)
}

fn restore_optional_bytes(path: &Path, original: Option<&[u8]>) -> Result<()> {
    match original {
        Some(bytes) => atomic_write(path, bytes),
        None => remove_optional(path),
    }
}

fn remove_empty_dir(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn write_lease(path: &Path, state: &RuntimeConfigLease) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(path, &serde_json::to_vec_pretty(state)?)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeSubagentRepairReason {
    Lease,
    GeneratedRoleFiles,
    RuntimeAttestationPolicy,
}

impl RuntimeSubagentRepairReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lease => "lease",
            Self::GeneratedRoleFiles => "generated_role_files",
            Self::RuntimeAttestationPolicy => "runtime_attestation_policy",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuntimeSubagentReconcileReport {
    pub repaired: bool,
    pub reasons: Vec<RuntimeSubagentRepairReason>,
}

/// Verifies the complete Codey-owned generated role documents, their lease
/// hashes and the child-runtime attestation policy. Any drift is rebuilt from
/// the current saved role matrix. Editable source constraints and user-owned
/// Codex files remain outside this repair boundary.
pub(crate) fn reconcile_runtime_subagent_roles(
    config: &CodeyConfig,
) -> Result<RuntimeSubagentReconcileReport> {
    let marker = lease_marker_path();
    let _runtime_config_lock = RuntimeConfigLock::acquire(&marker)?;
    reconcile_runtime_subagent_roles_at(config, &marker)
}

fn reconcile_runtime_subagent_roles_at(
    config: &CodeyConfig,
    marker: &Path,
) -> Result<RuntimeSubagentReconcileReport> {
    anyhow::ensure!(
        config.subagent_optimization,
        "当前 Codey 配置未启用子代理协作优化"
    );
    let state = fs::read(marker)
        .with_context(|| format!("读取 Codey Codex lease 失败：{}", marker.display()))
        .and_then(|contents| {
            serde_json::from_slice::<RuntimeConfigLease>(&contents)
                .with_context(|| format!("解析 Codey Codex lease 失败：{}", marker.display()))
        })?;
    anyhow::ensure!(
        state.subagent_optimization_applied,
        "当前 Codex 运行时未注册 Codey 子代理任务类型，需要重启 Codex"
    );

    let runtime_roles = runtime_subagent_roles(
        Some(&config.subagent_roles),
        &config.subagent_model,
        &config.subagent_reasoning_effort,
    );
    anyhow::ensure!(
        state.subagent_roles.keys().eq(runtime_roles.keys()),
        "Codey 子代理角色启用状态已变化，需要重启 Codex 以重新注册可用角色"
    );
    let constraints_dir = marker.with_file_name(CODEY_CONSTRAINTS_DIR);
    let fastctx_instructions = runtime_fastctx_instructions(&constraints_dir, &state)?;
    let plans = plan_runtime_agent_files(
        &constraints_dir,
        &runtime_roles,
        fastctx_instructions.as_deref(),
    )
    .context("预检 Codey 子代理运行时配置失败；未写入运行时配置")?;
    let expected_hashes = runtime_agent_plan_hashes(&plans);
    let runtime_home = runtime_home_for_lease(&state);
    let lease_matches = state.subagent_model == config.subagent_model
        && state.subagent_reasoning_effort == config.subagent_reasoning_effort
        && state.subagent_roles == runtime_roles
        && !state.runtime_home.as_os_str().is_empty()
        && state.runtime_agent_schema_version == RUNTIME_AGENT_SCHEMA_VERSION
        && state.runtime_agent_hashes == expected_hashes;
    let generated_files_match = runtime_agent_files_match(&plans)?;
    let runtime_policy_matches = crate::subagent_gate::runtime_subagent_policy_matches(
        &runtime_home,
        &runtime_roles,
        &expected_hashes,
    )?;
    let mut reasons = Vec::new();
    if !lease_matches {
        reasons.push(RuntimeSubagentRepairReason::Lease);
    }
    if !generated_files_match {
        reasons.push(RuntimeSubagentRepairReason::GeneratedRoleFiles);
    }
    if !runtime_policy_matches {
        reasons.push(RuntimeSubagentRepairReason::RuntimeAttestationPolicy);
    }
    if reasons.is_empty() {
        return Ok(RuntimeSubagentReconcileReport::default());
    }

    refresh_runtime_subagent_roles_at(config, marker)?;
    Ok(RuntimeSubagentReconcileReport {
        repaired: true,
        reasons,
    })
}

fn runtime_agent_files_match(plans: &[RuntimeAgentPlan]) -> Result<bool> {
    for plan in plans {
        let contents = match fs::read(&plan.registration.config_file) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "读取 Codey 子代理运行时文件失败：{}",
                        plan.registration.config_file.display()
                    )
                });
            }
        };
        if contents != plan.contents
            || crate::fs_util::sha256_hex(&contents) != plan.registration.content_sha256
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn runtime_agent_plan_hashes(plans: &[RuntimeAgentPlan]) -> BTreeMap<String, String> {
    plans
        .iter()
        .map(|plan| {
            (
                plan.registration.role.clone(),
                plan.registration.content_sha256.clone(),
            )
        })
        .collect()
}

fn runtime_fastctx_instructions(
    constraints_dir: &Path,
    state: &RuntimeConfigLease,
) -> Result<Option<String>> {
    if state.fastctx_command.is_some() {
        Ok(Some(read_or_create_constraint_file(
            &constraints_dir.join(CODEY_FASTCTX_INSTRUCTIONS_FILE),
            CODEY_FASTCTX_GUIDANCE,
        )?))
    } else {
        Ok(None)
    }
}

fn runtime_home_for_lease(state: &RuntimeConfigLease) -> PathBuf {
    if state.runtime_home.as_os_str().is_empty() {
        codex_home().to_path_buf()
    } else {
        state.runtime_home.clone()
    }
}

pub(crate) fn restore_runtime_config_for_router_mode(
    home: &Path,
    repair_router_config: bool,
) -> Result<bool> {
    let marker = lease_marker_path();
    let _runtime_config_lock = RuntimeConfigLock::acquire(&marker)?;
    restore_runtime_config_at(home, &marker, repair_router_config)
}

fn restore_runtime_config_at(
    home: &Path,
    marker: &Path,
    repair_router_config: bool,
) -> Result<bool> {
    let state = match fs::read_to_string(marker) {
        Ok(contents) => serde_json::from_str::<RuntimeConfigLease>(&contents)
            .with_context(|| format!("解析 Codey Codex lease 失败：{}", marker.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if repair_router_config {
                repair_persistent_codey_runtime_config(home)
            } else {
                Ok(false)
            };
        }
        Err(error) => return Err(error.into()),
    };
    let repair_router_config = repair_router_config || state.local_router_applied;
    if state.isolated_runtime_constraints {
        rollback_isolated_runtime_config(home, marker, &state)?;
        if repair_router_config {
            let _ = repair_persistent_codey_runtime_config(home)?;
        }
        return Ok(true);
    }
    restore_runtime_hooks_file(home, &state)?;
    restore_runtime_subagent_files(home, &state)?;
    crate::subagent_gate::clear_runtime_subagent_policy(home)?;
    remove_optional(marker)?;
    if repair_router_config {
        let _ = repair_persistent_codey_runtime_config(home)?;
    }
    Ok(true)
}

fn repair_persistent_codey_runtime_config(home: &Path) -> Result<bool> {
    let config_path = home.join("config.toml");
    let manager = ConfigManager::new(&config_path);
    let snapshot = manager.load()?;
    if !snapshot.exists() {
        return Ok(false);
    }
    let mut document = snapshot.document().clone();
    let codey_router_owned = codey_router_provider_is_codey_owned(&document);
    let codey_router_dangling = persistent_codey_router_selection_is_dangling(&document);
    let removed = remove_persistent_codey_runtime_config(&mut document, home);
    let shimmed = if user_owned_router_provider_occupies_id(&document) {
        false
    } else if codey_router_owned || codey_router_dangling {
        ensure_persistent_router_resume_shim(&mut document)?
    } else {
        false
    };
    if !removed && !shimmed {
        return Ok(false);
    }
    manager.replace_document(
        Some(snapshot.revision()),
        document,
        "repair legacy Codey runtime-only settings",
        "codex_config.repair_persistent_codey_runtime_config",
    )?;
    Ok(true)
}

/// Idle/restore disk table for `codey_router`. Codex Desktop looks up a
/// thread's saved `model_provider` in `config.toml`; older releases stamped
/// launch-only `codey_router` into rollout/SQLite, then deleted that table on
/// repair. A non-loopback, non-secret shim keeps those threads loadable when
/// Codey is not running, without selecting `codey_router` as the user's
/// provider. While the local router is up, [`prepare_runtime_router_disk_provider_at`]
/// replaces this with the live loopback API-key table so third-party catalog
/// aliases are not sent to ChatGPT-account transport.
pub fn prepare_persistent_router_resume_shim(home: &Path) -> Result<bool> {
    let marker = lease_marker_path();
    let _runtime_config_lock = RuntimeConfigLock::acquire(&marker)?;
    prepare_persistent_router_resume_shim_at(home)
}

pub(crate) fn prepare_persistent_router_resume_shim_at(home: &Path) -> Result<bool> {
    let config_path = home.join("config.toml");
    let manager = ConfigManager::new(&config_path);
    let snapshot = manager.load()?;
    if !snapshot.exists() {
        return Ok(false);
    }
    let mut document = snapshot.document().clone();
    if user_owned_router_provider_occupies_id(&document) {
        return Ok(false);
    }
    if !ensure_persistent_router_resume_shim(&mut document)? {
        return Ok(false);
    }
    manager.replace_document(
        Some(snapshot.revision()),
        document,
        "install persistent codey_router resume shim",
        "codex_config.prepare_persistent_router_resume_shim",
    )?;
    Ok(true)
}

/// Persist the live loopback `codey_router` table while the local router is
/// running. Official authentication stays enabled when available, while the
/// provider name advertises OpenAI-only capabilities only when every runtime
/// route supports them. Desktop and config reload resolve that id from disk,
/// so it must match the process `-c` overlay.
pub(crate) fn prepare_runtime_router_disk_provider_at(
    home: &Path,
    endpoint: &RuntimeRouterEndpoint,
) -> Result<bool> {
    let config_path = home.join("config.toml");
    let manager = ConfigManager::new(&config_path);
    let snapshot = manager.load()?;
    if !snapshot.exists() {
        return Ok(false);
    }
    let mut document = snapshot.document().clone();
    if user_owned_router_provider_occupies_id(&document) {
        return Ok(false);
    }
    let desired = local_router_provider_table(endpoint);
    if runtime_router_disk_provider_matches(&document, &desired) {
        return Ok(false);
    }
    write_persistent_router_shim(&mut document, desired)?;
    manager.replace_document(
        Some(snapshot.revision()),
        document,
        "install runtime codey_router loopback provider",
        "codex_config.prepare_runtime_router_disk_provider",
    )?;
    Ok(true)
}

fn remove_persistent_codey_runtime_config(doc: &mut DocumentMut, home: &Path) -> bool {
    let before = doc.to_string();
    let codey_router_owned = codey_router_provider_is_codey_owned(doc);
    let codey_subagent_owned = persistent_codey_subagent_config_is_owned(doc);
    let codey_router_selected = persistent_codey_router_is_selected(doc);
    let codey_router_runtime_selected =
        codey_router_selected && (codey_router_owned || codey_subagent_owned);
    let codey_router_dangling = persistent_codey_router_selection_is_dangling(doc);
    if codey_router_runtime_selected || codey_router_dangling {
        doc.as_table_mut().remove("model_provider");
    }
    if doc
        .get("model")
        .and_then(Item::as_str)
        .is_some_and(|_| codey_router_runtime_selected || codey_router_dangling)
    {
        doc.as_table_mut().remove("model");
    }
    if codey_router_owned || codey_router_dangling || codey_subagent_owned {
        remove_codey_model_catalog_reference(doc, home);
    }
    remove_codey_owned_agents_config(doc, codey_subagent_owned);
    remove_codey_owned_multi_agent_defaults(doc, codey_subagent_owned);
    doc.to_string() != before
}

pub(crate) fn user_owned_router_provider_occupies_id(document: &DocumentMut) -> bool {
    let Some(item) = document_provider_item(document, local_router::ROUTER_PROVIDER_ID) else {
        return false;
    };
    match item.as_table_like() {
        Some(provider) => !codey_router_provider_table_is_codey_owned(provider),
        None => true,
    }
}

fn codey_router_provider_is_codey_owned(doc: &DocumentMut) -> bool {
    document_provider_table(doc, local_router::ROUTER_PROVIDER_ID)
        .is_some_and(codey_router_provider_table_is_codey_owned)
}

fn codey_router_provider_table_is_codey_owned(provider: &dyn TableLike) -> bool {
    let has_codey_token_header = provider
        .get("http_headers")
        .and_then(Item::as_table_like)
        .is_some_and(|headers| headers.contains_key(local_router::ROUTER_AUTH_HEADER));
    let has_codey_name = table_like_str(provider, "name") == Some(LOCAL_ROUTER_PROVIDER_NAME);
    has_codey_token_header || has_codey_name
}

fn persistent_codey_router_is_selected(doc: &DocumentMut) -> bool {
    doc.get("model_provider")
        .and_then(Item::as_str)
        .is_some_and(|provider| provider.trim() == local_router::ROUTER_PROVIDER_ID)
}

fn persistent_codey_router_selection_is_dangling(doc: &DocumentMut) -> bool {
    persistent_codey_router_is_selected(doc)
        && document_provider_table(doc, local_router::ROUTER_PROVIDER_ID).is_none()
}

fn ensure_persistent_router_resume_shim(doc: &mut DocumentMut) -> Result<bool> {
    if user_owned_router_provider_occupies_id(doc) {
        return Ok(false);
    }
    let desired = persistent_router_shim_table(doc);
    if persistent_router_shim_matches(doc, &desired) {
        return Ok(false);
    }
    write_persistent_router_shim(doc, desired)?;
    Ok(true)
}

fn persistent_router_shim_table(doc: &DocumentMut) -> Table {
    let source = persistent_router_shim_source(doc);
    let source_base_url = source
        .and_then(|provider| table_like_str(provider, "base_url"))
        .filter(|url| !is_loopback_http_url(url));
    let base_url = source_base_url.unwrap_or(CHATGPT_CODEX_BASE_URL);
    let requires_openai_auth = source
        .and_then(|provider| table_like_bool(provider, "requires_openai_auth"))
        .unwrap_or(base_url == CHATGPT_CODEX_BASE_URL);
    let mut provider = Table::new();
    provider["name"] = value(LOCAL_ROUTER_PROVIDER_NAME);
    provider["base_url"] = value(base_url);
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(requires_openai_auth);
    provider["supports_websockets"] = value(false);
    provider
}

fn persistent_router_shim_source(doc: &DocumentMut) -> Option<&dyn TableLike> {
    if let Some(selected) = persistent_non_router_provider_id(doc)
        && let Some(provider) = document_provider_table(doc, selected)
    {
        return Some(provider);
    }
    unique_non_router_provider_table(doc)
}

fn persistent_non_router_provider_id(doc: &DocumentMut) -> Option<&str> {
    doc.get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty() && *provider != local_router::ROUTER_PROVIDER_ID)
}

fn unique_non_router_provider_table(doc: &DocumentMut) -> Option<&dyn TableLike> {
    let providers = doc.get("model_providers").and_then(Item::as_table_like)?;
    let mut other = None;
    for (key, item) in providers.iter() {
        if key == local_router::ROUTER_PROVIDER_ID {
            continue;
        }
        let Some(provider) = item.as_table_like() else {
            continue;
        };
        if other.is_some() {
            return None;
        }
        other = Some(provider);
    }
    other
}

fn runtime_router_disk_provider_matches(doc: &DocumentMut, desired: &Table) -> bool {
    let Some(existing) = document_provider_table(doc, local_router::ROUTER_PROVIDER_ID) else {
        return false;
    };
    if !codey_router_provider_table_is_codey_owned(existing) {
        return false;
    }
    table_like_str(existing, "name") == table_str(desired, "name")
        && table_like_str(existing, "base_url") == table_str(desired, "base_url")
        && table_like_str(existing, "wire_api") == table_str(desired, "wire_api")
        && table_like_bool(existing, "requires_openai_auth")
            == table_bool(desired, "requires_openai_auth")
        && table_like_bool(existing, "supports_websockets")
            == table_bool(desired, "supports_websockets")
        && table_like_str(existing, "experimental_bearer_token")
            == table_str(desired, "experimental_bearer_token")
        && provider_header_str(existing, local_router::ROUTER_AUTH_HEADER)
            == provider_header_str(desired, local_router::ROUTER_AUTH_HEADER)
}

fn provider_header_str<'a>(provider: &'a dyn TableLike, key: &str) -> Option<&'a str> {
    provider
        .get("http_headers")
        .and_then(Item::as_table_like)
        .and_then(|headers| table_like_str(headers, key))
}

fn persistent_router_shim_matches(doc: &DocumentMut, desired: &Table) -> bool {
    let Some(existing) = document_provider_table(doc, local_router::ROUTER_PROVIDER_ID) else {
        return false;
    };
    if !codey_router_provider_table_is_codey_owned(existing) {
        return false;
    }
    if provider_has_router_secret(existing) {
        return false;
    }
    let existing_url = table_like_str(existing, "base_url").unwrap_or_default();
    if is_loopback_http_url(existing_url) {
        return false;
    }
    table_like_str(existing, "name") == table_str(desired, "name")
        && table_like_str(existing, "base_url") == table_str(desired, "base_url")
        && table_like_str(existing, "wire_api") == table_str(desired, "wire_api")
        && table_like_bool(existing, "requires_openai_auth")
            == table_bool(desired, "requires_openai_auth")
        && table_like_bool(existing, "supports_websockets") == Some(false)
}

fn provider_has_router_secret(provider: &dyn TableLike) -> bool {
    let has_token_header = provider
        .get("http_headers")
        .and_then(Item::as_table_like)
        .is_some_and(|headers| headers.contains_key(local_router::ROUTER_AUTH_HEADER));
    let has_bearer = table_like_str(provider, "experimental_bearer_token").is_some();
    has_token_header || has_bearer
}

fn write_persistent_router_shim(doc: &mut DocumentMut, table: Table) -> Result<()> {
    match doc.get_mut("model_providers") {
        None => {
            let mut providers = Table::new();
            providers.insert(local_router::ROUTER_PROVIDER_ID, Item::Table(table));
            doc["model_providers"] = Item::Table(providers);
        }
        Some(item) => {
            if let Some(providers) = item.as_table_mut() {
                providers.insert(local_router::ROUTER_PROVIDER_ID, Item::Table(table));
            } else if let Some(providers) = item.as_inline_table_mut() {
                providers.insert(
                    local_router::ROUTER_PROVIDER_ID,
                    Value::InlineTable(table_values_to_inline(&table)),
                );
            } else {
                bail!("model_providers 必须是 TOML table");
            }
        }
    }
    Ok(())
}

fn table_values_to_inline(table: &Table) -> InlineTable {
    let mut inline = InlineTable::new();
    for (key, item) in table.iter() {
        if let Some(value) = item.as_value() {
            inline.insert(key, value.clone());
        }
    }
    inline
}

fn document_provider_item<'a>(doc: &'a DocumentMut, id: &str) -> Option<&'a Item> {
    doc.get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(id))
}

fn document_provider_table<'a>(doc: &'a DocumentMut, id: &str) -> Option<&'a dyn TableLike> {
    document_provider_item(doc, id).and_then(Item::as_table_like)
}

fn table_like_str<'a>(table: &'a dyn TableLike, key: &str) -> Option<&'a str> {
    table
        .get(key)
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn table_like_bool(table: &dyn TableLike, key: &str) -> Option<bool> {
    table.get(key).and_then(Item::as_bool)
}

fn table_str<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table
        .get(key)
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn table_bool(table: &Table, key: &str) -> Option<bool> {
    table.get(key).and_then(Item::as_bool)
}

fn is_loopback_http_url(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:")
        || url.starts_with("http://[::1]:")
        || url.starts_with("https://127.0.0.1:")
        || url.starts_with("https://localhost:")
        || url.starts_with("https://[::1]:")
}

fn is_route_qualified_model(model: &str) -> bool {
    model
        .trim()
        .rsplit_once('/')
        .is_some_and(|(route, source_model)| {
            !route.trim().is_empty() && !source_model.trim().is_empty()
        })
}

fn remove_codey_model_catalog_reference(doc: &mut DocumentMut, home: &Path) {
    let Some(catalog_path) = doc.get("model_catalog_json").and_then(Item::as_str) else {
        return;
    };
    let catalog_path = catalog_path.trim();
    let relative = crate::model_catalog::relative_path();
    let absolute = home.join(relative).to_string_lossy().replace('\\', "/");
    let normalized = catalog_path.replace('\\', "/");
    if normalized == relative
        || normalized == absolute
        || normalized.ends_with(&format!("/{relative}"))
    {
        doc.as_table_mut().remove("model_catalog_json");
    }
}

fn persistent_codey_subagent_config_is_owned(doc: &DocumentMut) -> bool {
    let owned_role = doc
        .get("agents")
        .and_then(Item::as_table)
        .is_some_and(|agents| {
            SUBAGENT_ROLE_IDS.iter().any(|role| {
                agents
                    .get(role)
                    .and_then(Item::as_table)
                    .is_some_and(agent_role_table_is_codey_owned)
            }) || SUBAGENT_ROLE_IDS
                .iter()
                .filter(|role| **role != SUBAGENT_ROLE_DEFAULT)
                .any(|role| agents.contains_key(role))
        });
    let owned_multi_agent_hint = doc
        .get("features")
        .and_then(Item::as_table)
        .and_then(|features| features.get("multi_agent_v2"))
        .and_then(Item::as_table)
        .is_some_and(|multi_agent| {
            multi_agent
                .get("multi_agent_mode_hint_text")
                .and_then(Item::as_str)
                == Some(ROOT_AGENT_MULTI_AGENT_MODE_HINT)
                || multi_agent
                    .get("root_agent_usage_hint_text")
                    .and_then(Item::as_str)
                    .is_some_and(|hint| {
                        ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS
                            .iter()
                            .any(|owned| hint.contains(owned.trim()))
                    })
        });
    owned_role || owned_multi_agent_hint
}

fn remove_codey_owned_agents_config(doc: &mut DocumentMut, codey_owned: bool) {
    let Some(agents) = doc.get_mut("agents").and_then(Item::as_table_mut) else {
        return;
    };
    for role in SUBAGENT_ROLE_IDS {
        let remove_role = agents
            .get(role)
            .and_then(Item::as_table)
            .is_some_and(agent_role_table_is_codey_owned)
            || (codey_owned && role != SUBAGENT_ROLE_DEFAULT);
        if remove_role {
            agents.remove(role);
        }
    }
    let removed_default_model = agents
        .get("default_subagent_model")
        .and_then(Item::as_str)
        .is_some_and(|model| codey_owned && is_route_qualified_model(model));
    if removed_default_model {
        agents.remove("default_subagent_model");
        agents.remove("default_subagent_reasoning_effort");
    }
    if agents.is_empty() {
        doc.as_table_mut().remove("agents");
    }
}

fn agent_role_table_is_codey_owned(role: &Table) -> bool {
    role.get("config_file")
        .and_then(Item::as_str)
        .is_some_and(|path| {
            let normalized = path.replace('\\', "/");
            normalized.contains("/codex-constraints/runtime/")
                || normalized.starts_with("codex-constraints/runtime/")
        })
}

fn remove_codey_owned_multi_agent_defaults(doc: &mut DocumentMut, codey_owned: bool) {
    if !codey_owned {
        return;
    }
    let Some(features) = doc.get_mut("features").and_then(Item::as_table_mut) else {
        return;
    };
    let Some(multi_agent) = features
        .get_mut("multi_agent_v2")
        .and_then(Item::as_table_mut)
    else {
        return;
    };
    if multi_agent
        .get("default_subagent_model")
        .and_then(Item::as_str)
        .is_some_and(is_route_qualified_model)
    {
        multi_agent.remove("default_subagent_model");
        multi_agent.remove("default_subagent_reasoning_effort");
    }
    if multi_agent.is_empty() {
        features.remove("multi_agent_v2");
    }
    if features.is_empty() {
        doc.as_table_mut().remove("features");
    }
}

fn restore_runtime_subagent_files(home: &Path, state: &RuntimeConfigLease) -> Result<()> {
    if !state.subagent_optimization_applied
        || state.isolated_runtime_constraints
        || state.independent_prompt_sources
    {
        return Ok(());
    }

    let agents_md_path = home.join("AGENTS.md");
    let original_agents_md = if state.original_agents_md_exists {
        Some(
            fs::read(state.backup_dir.join("AGENTS.md"))
                .context("找不到 Codex 原 AGENTS.md 租约快照")?,
        )
    } else {
        None
    };
    let applied_agents_md = fs::read(state.backup_dir.join(APPLIED_AGENTS_MD_FILE))
        .context("找不到 Codey 已应用 AGENTS.md 租约快照")?;
    restore_agents_md(
        &agents_md_path,
        original_agents_md.as_deref(),
        &applied_agents_md,
    )?;

    let agents_dir = home.join("agents");
    let default_agent_path = agents_dir.join("default.toml");
    let original_default_agent = if state.original_default_agent_exists {
        Some(
            fs::read(state.backup_dir.join("agents/default.toml"))
                .context("找不到 Codex 原 default.toml 租约快照")?,
        )
    } else {
        None
    };
    let applied_default_agent = fs::read(state.backup_dir.join(APPLIED_DEFAULT_AGENT_FILE))
        .context("找不到 Codey 已应用 default.toml 租约快照")?;
    restore_if_still_applied(
        &default_agent_path,
        original_default_agent.as_deref(),
        &applied_default_agent,
    )?;
    if !state.original_agents_dir_exists {
        remove_empty_dir(&agents_dir)?;
    }
    Ok(())
}

fn restore_runtime_hooks_file(home: &Path, state: &RuntimeConfigLease) -> Result<()> {
    if !state.runtime_hooks_applied {
        return Ok(());
    }
    let hooks_path = home.join("hooks.json");
    let Some(current) = read_optional(&hooks_path)? else {
        return Ok(());
    };
    let applied = fs::read(state.backup_dir.join(APPLIED_HOOKS_JSON_FILE))
        .context("找不到 Codey 已应用 hooks.json 租约快照")?;
    let original = if state.original_hooks_file_exists {
        Some(
            fs::read(state.backup_dir.join("hooks.json"))
                .context("找不到 Codex 原 hooks.json 租约快照")?,
        )
    } else {
        None
    };
    if current == applied {
        return restore_optional_bytes(&hooks_path, original.as_deref());
    }

    // A user may edit hooks.json while Codey is running. In that case remove
    // only Codey's identifiable command groups and retain all concurrent edits.
    let Ok(mut document) = serde_json::from_slice::<serde_json::Value>(&current) else {
        return Ok(());
    };
    let Some(root) = document.as_object_mut() else {
        return Ok(());
    };
    if let Some(hooks) = root
        .get_mut("hooks")
        .and_then(serde_json::Value::as_object_mut)
    {
        for groups in hooks.values_mut() {
            if let Some(groups) = groups.as_array_mut() {
                groups.retain(|group| !json_hook_group_is_codey_owned(group));
            }
        }
        hooks.retain(|_, groups| !groups.as_array().is_some_and(Vec::is_empty));
        if hooks.is_empty() && !state.original_hooks_file_exists {
            root.remove("hooks");
        }
    }
    if !state.original_hooks_file_exists
        && root.get("description").and_then(serde_json::Value::as_str)
            == Some(CODEY_HOOKS_DESCRIPTION)
    {
        root.remove("description");
    }
    if root.is_empty() && !state.original_hooks_file_exists {
        remove_optional(&hooks_path)
    } else {
        let mut rendered = serde_json::to_vec_pretty(&document)?;
        rendered.push(b'\n');
        atomic_write(&hooks_path, &rendered)
    }
}

fn restore_agents_md(path: &Path, original: Option<&[u8]>, applied: &[u8]) -> Result<()> {
    let Some(current) = read_optional(path)? else {
        return Ok(());
    };
    if current == applied {
        return restore_optional_bytes(path, original);
    }
    let original_contains_guidance = original
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .is_some_and(|contents| contents.contains(SUBAGENT_GUIDANCE));
    if original_contains_guidance {
        return Ok(());
    }
    let current = String::from_utf8(current).context("Codex 当前 AGENTS.md 不是 UTF-8")?;
    let Some(restored) = remove_subagent_guidance(&current) else {
        return Ok(());
    };
    if original.is_none() && restored.trim().is_empty() {
        remove_optional(path)
    } else {
        atomic_write(path, restored.as_bytes())
    }
}

fn restore_if_still_applied(path: &Path, original: Option<&[u8]>, applied: &[u8]) -> Result<()> {
    if read_optional(path)?.as_deref() == Some(applied) {
        restore_optional_bytes(path, original)?;
    }
    Ok(())
}

fn fastctx_table_server_is_codey_owned(server: &Table) -> bool {
    server
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(arguments_have_codey_fastctx_marker)
}

fn ensure_child_table<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    if parent.get(key).is_none() {
        parent.insert(key, Item::Table(Table::new()));
    }
    parent
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("{key} 必须是 TOML table"))
}

/// Reads the provider bucket selected by the current Codex configuration.
/// Codex defaults to its built-in `openai` provider when the root key is absent.
pub fn current_model_provider(home: &Path) -> Result<String> {
    let config_path = home.join("config.toml");
    let original = read_codex_config(&config_path)?;
    let existing =
        String::from_utf8(original.unwrap_or_default()).context("Codex config.toml 不是 UTF-8")?;
    let doc = parse_document(&existing)?;
    Ok(doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or(BUILTIN_OPENAI_PROVIDER_ID)
        .to_string())
}

pub(crate) fn fast_context_tools_status(home: &Path) -> Result<FastContextToolsStatus> {
    let config_path = home.join("config.toml");
    let original = read_codex_config(&config_path)?;
    let existing =
        String::from_utf8(original.unwrap_or_default()).context("Codex config.toml 不是 UTF-8")?;
    let document = parse_document(&existing)?;
    Ok(fast_context_tools_status_from_document(&document))
}

#[cfg(test)]
pub fn patch_config(existing: &str, use_official_catalog: bool) -> Result<String> {
    let model_catalog_path =
        use_official_catalog.then(|| Path::new(crate::model_catalog::relative_path()));
    patch_config_with_fastctx(existing, model_catalog_path, None, None, false)
}

#[cfg(test)]
fn patch_config_with_fastctx(
    existing: &str,
    model_catalog_path: Option<&Path>,
    default_model: Option<&str>,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
) -> Result<String> {
    patch_config_with_fastctx_mode(
        existing,
        RouterPatchOptions {
            config_path: Path::new("config.toml"),
            model_catalog_path,
            default_model,
            fastctx_command,
            subagent_optimization,
            subagent_model: DEFAULT_SUBAGENT_MODEL,
            subagent_reasoning_effort: DEFAULT_SUBAGENT_REASONING_EFFORT,
            local_router: Some(test_runtime_router_endpoint()),
        },
    )
}

#[cfg(test)]
fn test_runtime_router_endpoint() -> &'static RuntimeRouterEndpoint {
    static ENDPOINT: OnceLock<RuntimeRouterEndpoint> = OnceLock::new();
    ENDPOINT.get_or_init(|| RuntimeRouterEndpoint {
        base_url: "http://127.0.0.1:43127/v1".to_string(),
        token: "test-router-token".to_string(),
        supports_websockets: false,
        supports_remote_compaction: false,
        requires_openai_auth: false,
    })
}

struct RouterPatchOptions<'a> {
    config_path: &'a Path,
    model_catalog_path: Option<&'a Path>,
    default_model: Option<&'a str>,
    fastctx_command: Option<&'a Path>,
    subagent_optimization: bool,
    subagent_model: &'a str,
    subagent_reasoning_effort: &'a str,
    local_router: Option<&'a RuntimeRouterEndpoint>,
}

fn patch_config_with_fastctx_mode(
    existing: &str,
    options: RouterPatchOptions<'_>,
) -> Result<String> {
    let RouterPatchOptions {
        config_path,
        model_catalog_path,
        default_model,
        fastctx_command,
        subagent_optimization,
        subagent_model,
        subagent_reasoning_effort,
        local_router,
    } = options;
    let mut doc = parse_document(existing)?;
    if let Some(local_router) = local_router {
        ensure_provider_table(&mut doc)?;
        doc["model_providers"]
            .as_table_mut()
            .expect("model_providers was initialized")[local_router::ROUTER_PROVIDER_ID] =
            Item::Table(local_router_provider_table(local_router));
        doc["model_provider"] = value(local_router::ROUTER_PROVIDER_ID);
        update_model_catalog_reference(&mut doc, config_path, model_catalog_path);
        set_model_selection(&mut doc, default_model);
    }
    enable_desktop_reasoning_efforts(&mut doc)?;
    ensure_default_service_tier(&mut doc);
    let fastctx_namespace = if let Some(command) = fastctx_command {
        enable_fast_context_tools(&mut doc, command)?
    } else {
        disable_fast_context_tools(&mut doc);
        None
    };
    if subagent_optimization {
        enable_subagent_optimization(
            &mut doc,
            config_path,
            subagent_model,
            subagent_reasoning_effort,
            fastctx_namespace.as_deref(),
        )?;
    }
    if fastctx_namespace.is_some() {
        enable_hooks_feature(&mut doc)?;
        if !subagent_optimization {
            enable_fastctx_route_hook(&mut doc, config_path)?;
        }
    }
    document_string(&doc)
}

fn enable_subagent_optimization(
    doc: &mut DocumentMut,
    config_path: &Path,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
    fastctx_namespace: Option<&str>,
) -> Result<()> {
    let subagent_model = subagent_model.trim();
    if subagent_model.is_empty() {
        bail!("子代理模型不能为空");
    }
    let subagent_reasoning_effort = subagent_reasoning_effort.trim().to_ascii_lowercase();
    if !SUBAGENT_REASONING_EFFORTS.contains(&subagent_reasoning_effort.as_str()) {
        bail!("子代理思考深度无效：{subagent_reasoning_effort}");
    }
    let inherited_developer_instructions = doc
        .get("developer_instructions")
        .and_then(Item::as_str)
        .unwrap_or_default()
        .to_string();
    let migrate_previous_owned_concurrency = doc
        .get("agents")
        .and_then(Item::as_table)
        .is_some_and(|agents| {
            agents
                .get("max_concurrent_threads_per_session")
                .and_then(Item::as_integer)
                == Some(PREVIOUS_DEFAULT_SUBAGENT_MAX_CONCURRENCY)
                && agents.get("codey_quick_scan").is_some()
                && agents.get("codey_worker").is_some()
        })
        && doc
            .get("features")
            .and_then(Item::as_table)
            .and_then(|features| features.get("multi_agent_v2"))
            .and_then(Item::as_table)
            .and_then(|multi_agent| multi_agent.get("tool_namespace"))
            .and_then(Item::as_str)
            == Some("agents");
    let agents = ensure_root_table(doc, "agents")?;
    let legacy_max_threads = agents.remove("max_threads");
    agents.remove("max_depth");
    agents["enabled"] = value(true);
    let has_valid_concurrency = agents
        .get("max_concurrent_threads_per_session")
        .and_then(Item::as_integer)
        .is_some_and(|concurrency| concurrency > 0);
    if migrate_previous_owned_concurrency {
        agents["max_concurrent_threads_per_session"] = value(DEFAULT_SUBAGENT_MAX_CONCURRENCY);
    } else if !has_valid_concurrency {
        agents["max_concurrent_threads_per_session"] = legacy_max_threads
            .filter(|legacy| {
                legacy
                    .as_integer()
                    .is_some_and(|concurrency| concurrency > 0)
            })
            .unwrap_or_else(|| value(DEFAULT_SUBAGENT_MAX_CONCURRENCY));
    }
    agents["default_subagent_model"] = value(subagent_model);
    agents["default_subagent_reasoning_effort"] = value(subagent_reasoning_effort);
    let features = ensure_root_table(doc, "features")?;
    if features.get("multi_agent_v2").is_none() {
        features["multi_agent_v2"] = Item::Table(Table::new());
    }
    let multi_agent = features["multi_agent_v2"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("features.multi_agent_v2 必须是 TOML table"))?;
    // Keep the v2 flag for current runtime compatibility; the public subagent
    // settings now live under [agents].
    multi_agent["enabled"] = value(true);
    multi_agent["wait_agent_enabled"] = value(true);
    multi_agent["hide_spawn_agent_metadata"] = value(true);
    multi_agent["expose_spawn_agent_model_overrides"] = value(false);
    multi_agent["tool_namespace"] = value("agents");
    for migrated_key in [
        "max_concurrent_threads_per_session",
        "default_subagent_model",
        "default_subagent_reasoning_effort",
    ] {
        multi_agent.remove(migrated_key);
    }
    multi_agent["min_wait_timeout_ms"] = value(10_000);
    multi_agent["default_wait_timeout_ms"] = value(30_000);
    multi_agent["max_wait_timeout_ms"] = value(30_000);
    let existing_root_usage_hint = multi_agent
        .get("root_agent_usage_hint_text")
        .map(|item| {
            item.as_str().ok_or_else(|| {
                anyhow::anyhow!("features.multi_agent_v2.root_agent_usage_hint_text 必须是字符串")
            })
        })
        .transpose()?
        .unwrap_or_default();
    multi_agent["root_agent_usage_hint_text"] = value(append_root_agent_collaboration_usage_hint(
        existing_root_usage_hint,
    ));
    multi_agent["multi_agent_mode_hint_text"] = value(ROOT_AGENT_MULTI_AGENT_MODE_HINT);
    if let Some(namespace) = fastctx_namespace {
        if multi_agent.get("subagent_developer_instructions").is_none() {
            multi_agent["subagent_developer_instructions"] =
                value(inherited_developer_instructions);
        }
        apply_fastctx_guidance_to_table(
            multi_agent,
            "subagent_developer_instructions",
            namespace,
            "features.multi_agent_v2.subagent_developer_instructions",
        )?;
    }
    features["hooks"] = value(true);
    enable_subagent_gate_hooks(doc, config_path, fastctx_namespace.is_some())?;
    Ok(())
}

#[derive(Clone, Copy)]
struct CodeyHookSpec {
    toml_event: &'static str,
    event_key: &'static str,
    matcher: Option<&'static str>,
    timeout_seconds: u64,
}

struct RuntimeHookTrustEntry {
    state_key: String,
    trusted_hash: String,
}

struct RuntimeHooksFile {
    contents: Vec<u8>,
    trust_entries: Vec<RuntimeHookTrustEntry>,
}

const SUBAGENT_GATE_HOOKS: [CodeyHookSpec; 7] = [
    CodeyHookSpec {
        toml_event: "PreToolUse",
        event_key: "pre_tool_use",
        matcher: Some("*"),
        timeout_seconds: crate::subagent_gate::HOOK_TIMEOUT_SECONDS,
    },
    CodeyHookSpec {
        toml_event: "PostToolUse",
        event_key: "post_tool_use",
        matcher: Some(crate::subagent_orchestrator::POST_TOOL_HOOK_MATCHER),
        timeout_seconds: crate::subagent_gate::HOOK_TIMEOUT_SECONDS,
    },
    CodeyHookSpec {
        toml_event: "UserPromptSubmit",
        event_key: "user_prompt_submit",
        matcher: None,
        timeout_seconds: crate::subagent_gate::HOOK_TIMEOUT_SECONDS,
    },
    CodeyHookSpec {
        toml_event: "SubagentStart",
        event_key: "subagent_start",
        matcher: None,
        timeout_seconds: crate::subagent_gate::HOOK_TIMEOUT_SECONDS,
    },
    CodeyHookSpec {
        toml_event: "SubagentStop",
        event_key: "subagent_stop",
        matcher: None,
        timeout_seconds: crate::subagent_gate::HOOK_TIMEOUT_SECONDS,
    },
    CodeyHookSpec {
        toml_event: "Stop",
        event_key: "stop",
        matcher: None,
        timeout_seconds: crate::subagent_gate::HOOK_TIMEOUT_SECONDS,
    },
    CodeyHookSpec {
        toml_event: "SessionEnd",
        event_key: "session_end",
        matcher: None,
        timeout_seconds: crate::subagent_gate::SESSION_END_HOOK_TIMEOUT_SECONDS,
    },
];

const FASTCTX_ROUTE_HOOKS: [CodeyHookSpec; 1] = [CodeyHookSpec {
    toml_event: "PreToolUse",
    event_key: "pre_tool_use",
    matcher: Some(crate::fastctx_route_gate::HOOK_MATCHER),
    timeout_seconds: crate::fastctx_route_gate::HOOK_TIMEOUT_SECONDS,
}];

const CODEY_HOOK_EVENTS: [&str; 7] = [
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "SessionEnd",
];

fn build_runtime_hooks_file(
    existing: Option<&[u8]>,
    hooks_path: &Path,
    subagent_commands: Option<&crate::subagent_gate::HookCommands>,
    fastctx_commands: Option<&crate::subagent_gate::HookCommands>,
    combined_commands: Option<&crate::subagent_gate::HookCommands>,
) -> Result<RuntimeHooksFile> {
    let mut root = match existing {
        Some(existing) => serde_json::from_slice::<serde_json::Value>(existing)
            .context("解析 Codex hooks.json 失败")?,
        None => serde_json::json!({
            "description": CODEY_HOOKS_DESCRIPTION,
            "hooks": {},
        }),
    };
    let root = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Codex hooks.json 顶层必须是 JSON object"))?;
    if !root.contains_key("hooks") {
        root.insert("hooks".to_string(), serde_json::json!({}));
    }
    let hooks = root
        .get_mut("hooks")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| anyhow::anyhow!("Codex hooks.json 的 hooks 必须是 JSON object"))?;
    for groups in hooks.values_mut() {
        if let Some(groups) = groups.as_array_mut() {
            groups.retain(|group| !json_hook_group_is_codey_owned(group));
        }
    }

    let mut hook_plans = Vec::with_capacity(3);
    if let Some(subagent_commands) = subagent_commands {
        if let Some(combined_commands) = combined_commands {
            hook_plans.push((&SUBAGENT_GATE_HOOKS[..1], combined_commands));
            hook_plans.push((&SUBAGENT_GATE_HOOKS[1..], subagent_commands));
        } else {
            hook_plans.push((&SUBAGENT_GATE_HOOKS[..], subagent_commands));
        }
    }
    if let Some(fastctx_commands) = fastctx_commands {
        hook_plans.push((&FASTCTX_ROUTE_HOOKS[..], fastctx_commands));
    }
    let mut trust_entries =
        Vec::with_capacity(hook_plans.iter().map(|(specs, _)| specs.len()).sum());
    for (specs, commands) in hook_plans {
        let selected_command = if cfg!(windows) {
            commands.command_windows.as_str()
        } else {
            commands.command.as_str()
        };
        for &spec in specs {
            let groups = hooks
                .entry(spec.toml_event.to_string())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| {
                    anyhow::anyhow!("hooks.json hooks.{} 必须是数组", spec.toml_event)
                })?;
            let group_index = groups.len();
            let mut group = serde_json::Map::new();
            if let Some(matcher) = spec.matcher {
                group.insert(
                    "matcher".to_string(),
                    serde_json::Value::String(matcher.to_string()),
                );
            }
            group.insert(
                "hooks".to_string(),
                serde_json::json!([{
                    "type": "command",
                    "command": commands.command,
                    "commandWindows": commands.command_windows,
                    "timeout": spec.timeout_seconds,
                }]),
            );
            groups.push(serde_json::Value::Object(group));

            let state_key = format!(
                "{}:{}:{group_index}:0",
                hooks_path.display(),
                spec.event_key
            );
            let trusted_hash = crate::subagent_gate::hook_trust_hash(
                spec.event_key,
                spec.matcher,
                selected_command,
                spec.timeout_seconds,
            );
            trust_entries.push(RuntimeHookTrustEntry {
                state_key,
                trusted_hash,
            });
        }
    }
    let mut rendered = serde_json::to_vec_pretty(&serde_json::Value::Object(root.clone()))?;
    rendered.push(b'\n');
    Ok(RuntimeHooksFile {
        contents: rendered,
        trust_entries,
    })
}

fn json_hook_group_is_codey_owned(group: &serde_json::Value) -> bool {
    group
        .get("hooks")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|handlers| {
            handlers.iter().any(|handler| {
                ["command", "commandWindows", "command_windows"]
                    .into_iter()
                    .filter_map(|key| handler.get(key).and_then(serde_json::Value::as_str))
                    .any(hook_command_is_codey_owned)
            })
        })
}

#[allow(clippy::too_many_arguments)]
fn build_isolated_runtime_overrides(
    effective: &DocumentMut,
    root_instructions: Option<&str>,
    runtime_agents: &[RuntimeAgentRegistration],
    model_catalog_path: Option<&Path>,
    fastctx_namespace: Option<&str>,
    provider_id: Option<&str>,
    hook_trust_entries: &[RuntimeHookTrustEntry],
) -> Result<Vec<String>> {
    let mut overrides = Vec::new();
    push_required_document_override(
        &mut overrides,
        effective,
        &["desktop", "enabled-reasoning-efforts"],
        "desktop.enabled-reasoning-efforts",
    )?;
    push_required_document_override(&mut overrides, effective, &["service_tier"], "service_tier")?;

    if provider_id.is_some() {
        push_required_document_override(
            &mut overrides,
            effective,
            &["model_provider"],
            "model_provider",
        )?;
    }
    push_document_override(&mut overrides, effective, &["model"], "model")?;

    if model_catalog_path.is_some() {
        push_document_override(
            &mut overrides,
            effective,
            &["model_catalog_json"],
            "model_catalog_json",
        )?;
    }

    // The only runtime provider is Codey's process-local loopback gateway.
    // Upstream route tables and credentials never enter Codex's configuration.
    if let Some(provider_id) = provider_id {
        let provider_segment =
            codex_config_override_bare_segment(provider_id, "Codex Provider ID")?;
        for field in [
            "name",
            "base_url",
            "wire_api",
            "requires_openai_auth",
            "supports_websockets",
            "http_headers",
        ] {
            push_required_document_override(
                &mut overrides,
                effective,
                &["model_providers", provider_id, field],
                &format!("model_providers.{provider_segment}.{field}"),
            )?;
        }
        push_document_override(
            &mut overrides,
            effective,
            &["model_providers", provider_id, "experimental_bearer_token"],
            &format!("model_providers.{provider_segment}.experimental_bearer_token"),
        )?;
    }

    if fastctx_namespace.is_some() {
        for (path, key) in [
            (
                &["mcp_servers", CODEY_FASTCTX_SERVER_ID, "command"][..],
                "mcp_servers.codey_fastctx.command",
            ),
            (
                &["mcp_servers", CODEY_FASTCTX_SERVER_ID, "args"][..],
                "mcp_servers.codey_fastctx.args",
            ),
            (
                &[
                    "mcp_servers",
                    CODEY_FASTCTX_SERVER_ID,
                    "startup_timeout_sec",
                ][..],
                "mcp_servers.codey_fastctx.startup_timeout_sec",
            ),
            (
                &["mcp_servers", CODEY_FASTCTX_SERVER_ID, "tool_timeout_sec"][..],
                "mcp_servers.codey_fastctx.tool_timeout_sec",
            ),
            (
                &[
                    "mcp_servers",
                    CODEY_FASTCTX_SERVER_ID,
                    "env",
                    "FASTCTX_TOKEN_BUDGET",
                ][..],
                "mcp_servers.codey_fastctx.env.FASTCTX_TOKEN_BUDGET",
            ),
            (
                &[
                    "mcp_servers",
                    CODEY_FASTCTX_SERVER_ID,
                    "env",
                    "FASTCTX_GREP_TOKEN_BUDGET",
                ][..],
                "mcp_servers.codey_fastctx.env.FASTCTX_GREP_TOKEN_BUDGET",
            ),
            (
                &[
                    "mcp_servers",
                    CODEY_FASTCTX_SERVER_ID,
                    "env",
                    "FASTCTX_GLOB_TOKEN_BUDGET",
                ][..],
                "mcp_servers.codey_fastctx.env.FASTCTX_GLOB_TOKEN_BUDGET",
            ),
            (&["tool_output_token_limit"][..], "tool_output_token_limit"),
        ] {
            push_required_document_override(&mut overrides, effective, path, key)?;
        }
        push_document_override(
            &mut overrides,
            effective,
            &["features", "code_mode", "direct_only_tool_namespaces"],
            "features.code_mode.direct_only_tool_namespaces",
        )?;
    }

    if fastctx_namespace.is_some() || root_instructions.is_some() {
        push_document_override(
            &mut overrides,
            effective,
            &["developer_instructions"],
            "developer_instructions",
        )?;
    }

    if !runtime_agents.is_empty() {
        if fastctx_namespace.is_none() {
            push_document_override(
                &mut overrides,
                effective,
                &["features", "code_mode", "direct_only_tool_namespaces"],
                "features.code_mode.direct_only_tool_namespaces",
            )?;
        }
        for (path, key) in [
            (&["agents", "enabled"][..], "agents.enabled"),
            (
                &["agents", "max_concurrent_threads_per_session"][..],
                "agents.max_concurrent_threads_per_session",
            ),
            (
                &["agents", "default_subagent_model"][..],
                "agents.default_subagent_model",
            ),
            (
                &["agents", "default_subagent_reasoning_effort"][..],
                "agents.default_subagent_reasoning_effort",
            ),
            (
                &["features", "multi_agent_v2", "enabled"][..],
                "features.multi_agent_v2.enabled",
            ),
            (
                &["features", "multi_agent_v2", "wait_agent_enabled"][..],
                "features.multi_agent_v2.wait_agent_enabled",
            ),
            (
                &["features", "multi_agent_v2", "hide_spawn_agent_metadata"][..],
                "features.multi_agent_v2.hide_spawn_agent_metadata",
            ),
            (
                &[
                    "features",
                    "multi_agent_v2",
                    "expose_spawn_agent_model_overrides",
                ][..],
                "features.multi_agent_v2.expose_spawn_agent_model_overrides",
            ),
            (
                &["features", "multi_agent_v2", "tool_namespace"][..],
                "features.multi_agent_v2.tool_namespace",
            ),
            (
                &["features", "multi_agent_v2", "min_wait_timeout_ms"][..],
                "features.multi_agent_v2.min_wait_timeout_ms",
            ),
            (
                &["features", "multi_agent_v2", "default_wait_timeout_ms"][..],
                "features.multi_agent_v2.default_wait_timeout_ms",
            ),
            (
                &["features", "multi_agent_v2", "max_wait_timeout_ms"][..],
                "features.multi_agent_v2.max_wait_timeout_ms",
            ),
            (
                &["features", "multi_agent_v2", "root_agent_usage_hint_text"][..],
                "features.multi_agent_v2.root_agent_usage_hint_text",
            ),
            (
                &["features", "multi_agent_v2", "multi_agent_mode_hint_text"][..],
                "features.multi_agent_v2.multi_agent_mode_hint_text",
            ),
        ] {
            push_required_document_override(&mut overrides, effective, path, key)?;
        }
        if fastctx_namespace.is_some() {
            push_required_document_override(
                &mut overrides,
                effective,
                &[
                    "features",
                    "multi_agent_v2",
                    "subagent_developer_instructions",
                ],
                "features.multi_agent_v2.subagent_developer_instructions",
            )?;
        }
        for registration in runtime_agents {
            push_runtime_override_value(
                &mut overrides,
                &format!("agents.{}.config_file", registration.role),
                &Value::from(registration.config_file.to_string_lossy().into_owned()),
            );
            push_runtime_override_value(
                &mut overrides,
                &format!("agents.{}.description", registration.role),
                &Value::from(registration.description.as_str()),
            );
        }
    }
    if !hook_trust_entries.is_empty() {
        push_required_document_override(
            &mut overrides,
            effective,
            &["features", "hooks"],
            "features.hooks",
        )?;
        let expected_hook_count = if runtime_agents.is_empty() {
            usize::from(fastctx_namespace.is_some())
        } else {
            SUBAGENT_GATE_HOOKS.len()
        };
        anyhow::ensure!(
            hook_trust_entries.len() == expected_hook_count,
            "Codey Hook 信任项不完整"
        );
        for trust_entry in hook_trust_entries {
            let state_segment = toml_string_literal(&trust_entry.state_key);
            let key = format!("hooks.state.{state_segment}.trusted_hash");
            push_runtime_override_value(
                &mut overrides,
                &key,
                &Value::from(trust_entry.trusted_hash.as_str()),
            );
        }
    }
    if let Some(provider_id) = provider_id {
        validate_runtime_router_overrides(&overrides, provider_id)?;
    }
    Ok(overrides)
}

fn document_item_at<'a>(document: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    let mut current = document.get(first)?;
    for segment in rest {
        current = current.as_table_like()?.get(segment)?;
    }
    Some(current)
}

fn push_document_override(
    overrides: &mut Vec<String>,
    document: &DocumentMut,
    path: &[&str],
    key: &str,
) -> Result<()> {
    let Some(item) = document_item_at(document, path) else {
        return Ok(());
    };
    let value = item
        .as_value()
        .ok_or_else(|| anyhow::anyhow!("Codey 运行时覆盖项 {key} 必须是 TOML value"))?;
    push_runtime_override_value(overrides, key, value);
    Ok(())
}

fn push_required_document_override(
    overrides: &mut Vec<String>,
    document: &DocumentMut,
    path: &[&str],
    key: &str,
) -> Result<()> {
    anyhow::ensure!(
        document_item_at(document, path).is_some(),
        "Codey 必需运行时覆盖项缺失：{key}"
    );
    push_document_override(overrides, document, path, key)
}

fn push_runtime_override_value(overrides: &mut Vec<String>, key: &str, value: &Value) {
    let mut value = value.clone();
    value.decor_mut().clear();
    overrides.push(format!("{key}={value}"));
}

fn runtime_override_key(config: &str) -> &str {
    config
        .split_once('=')
        .map(|(key, _)| key)
        .unwrap_or(config)
        .trim()
}

fn validate_runtime_router_overrides(overrides: &[String], provider_id: &str) -> Result<()> {
    let provider_segment = codex_config_override_bare_segment(provider_id, "Codex Provider ID")?;
    let selected_router = overrides.iter().any(|entry| {
        runtime_override_key(entry) == "model_provider"
            && entry
                .split_once('=')
                .is_some_and(|(_, value)| value.trim() == toml_string_literal(provider_id))
    });
    if !selected_router {
        return Ok(());
    }

    let required_keys = [
        format!("model_providers.{provider_segment}.name"),
        format!("model_providers.{provider_segment}.base_url"),
        format!("model_providers.{provider_segment}.wire_api"),
        format!("model_providers.{provider_segment}.requires_openai_auth"),
        format!("model_providers.{provider_segment}.supports_websockets"),
        format!("model_providers.{provider_segment}.http_headers"),
    ];
    let missing = required_keys
        .iter()
        .filter(|key| {
            !overrides
                .iter()
                .any(|entry| runtime_override_key(entry) == key.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "Codey 运行时 Provider 覆盖项不完整：model_provider 选择了 {provider_id}，但缺少 {}",
        missing.join(", ")
    );
    Ok(())
}

fn toml_string_literal(value: &str) -> String {
    Value::from(value).to_string()
}

/// Codex parses the key portion of `-c key=value` as a plain dotted path, not
/// as TOML. Quoting a path segment therefore makes the quote characters part
/// of the provider id and leaves the real provider undefined. Codey-owned ids
/// already use this bare-key character set; reject imported ids that cannot be
/// represented safely instead of launching Codex with a dangling provider.
fn codex_config_override_bare_segment<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    anyhow::ensure!(
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "{label}「{value}」包含 Codex 临时配置路径不支持的字符"
    );
    Ok(value)
}

fn append_constraint_text(existing: &str, addition: &str) -> String {
    let existing = existing.trim();
    let addition = addition.trim();
    if addition.is_empty() || existing.contains(addition) {
        return existing.to_string();
    }
    if existing.is_empty() {
        addition.to_string()
    } else {
        format!("{existing}\n\n{addition}")
    }
}

fn enable_subagent_gate_hooks(
    doc: &mut DocumentMut,
    config_path: &Path,
    include_fastctx: bool,
) -> Result<()> {
    remove_codey_hooks(doc, config_path, &CODEY_HOOK_EVENTS)?;
    let commands = crate::subagent_gate::hook_commands()?;
    if !include_fastctx {
        return enable_codey_hooks(doc, config_path, &SUBAGENT_GATE_HOOKS, &commands);
    }
    let combined_commands =
        crate::subagent_gate::hook_commands_for(crate::subagent_gate::COMBINED_HOOK_ARGUMENT)?;
    enable_codey_hooks(
        doc,
        config_path,
        &SUBAGENT_GATE_HOOKS[..1],
        &combined_commands,
    )?;
    enable_codey_hooks(doc, config_path, &SUBAGENT_GATE_HOOKS[1..], &commands)
}

fn enable_hooks_feature(doc: &mut DocumentMut) -> Result<()> {
    if doc.get("features").is_none() {
        doc["features"] = Item::Table(Table::new());
    }
    let features = doc
        .get_mut("features")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| anyhow::anyhow!("features 必须是 TOML table 或 inline table"))?;
    features.insert("hooks", value(true));
    Ok(())
}

fn enable_fastctx_route_hook(doc: &mut DocumentMut, config_path: &Path) -> Result<()> {
    remove_codey_hooks(doc, config_path, &["PreToolUse"])?;
    let commands =
        crate::subagent_gate::hook_commands_for(crate::fastctx_route_gate::HOOK_ARGUMENT)?;
    enable_codey_hooks(doc, config_path, &FASTCTX_ROUTE_HOOKS, &commands)
}

fn remove_codey_hooks(
    doc: &mut DocumentMut,
    config_path: &Path,
    replacement_events: &[&str],
) -> Result<()> {
    let Some(hooks) = doc.get_mut("hooks") else {
        return Ok(());
    };
    let hooks = hooks
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("hooks 必须是 TOML table"))?;

    for (toml_event, event_key) in [
        ("PreToolUse", "pre_tool_use"),
        ("PostToolUse", "post_tool_use"),
        ("UserPromptSubmit", "user_prompt_submit"),
        ("SubagentStart", "subagent_start"),
        ("SubagentStop", "subagent_stop"),
        ("Stop", "stop"),
        ("SessionEnd", "session_end"),
    ] {
        let Some((index_map, remove_event)) = hooks
            .get_mut(toml_event)
            .map(remove_codey_hook_groups)
            .transpose()?
            .flatten()
        else {
            continue;
        };
        if remove_event && !replacement_events.contains(&toml_event) {
            hooks.remove(toml_event);
        }
        if let Some(state) = hooks.get_mut("state").and_then(Item::as_table_mut) {
            remap_hook_state_entries(state, config_path, event_key, &index_map);
        }
    }
    Ok(())
}

fn remove_codey_hook_groups(event: &mut Item) -> Result<Option<(Vec<Option<usize>>, bool)>> {
    let (owned, group_count) = match event {
        Item::ArrayOfTables(groups) => (
            groups
                .iter()
                .map(table_hook_group_is_codey_owned)
                .collect::<Vec<_>>(),
            groups.len(),
        ),
        Item::Value(Value::Array(groups)) => (
            groups
                .iter()
                .map(value_hook_group_is_codey_owned)
                .collect::<Vec<_>>(),
            groups.len(),
        ),
        _ => bail!("Hook 事件必须是配置数组"),
    };
    if !owned.iter().any(|owned| *owned) {
        return Ok(None);
    }

    let mut next_index = 0;
    let index_map = owned
        .iter()
        .map(|owned| {
            if *owned {
                None
            } else {
                let index = next_index;
                next_index += 1;
                Some(index)
            }
        })
        .collect::<Vec<_>>();
    for index in (0..group_count).rev() {
        if !owned[index] {
            continue;
        }
        match event {
            Item::ArrayOfTables(groups) => {
                groups.remove(index);
            }
            Item::Value(Value::Array(groups)) => {
                groups.remove(index);
            }
            _ => unreachable!("Hook event shape was validated"),
        }
    }
    Ok(Some((index_map, next_index == 0)))
}

fn table_hook_group_is_codey_owned(group: &Table) -> bool {
    group
        .get("hooks")
        .is_some_and(hook_handlers_item_is_codey_owned)
}

fn value_hook_group_is_codey_owned(group: &Value) -> bool {
    group
        .as_inline_table()
        .and_then(|group| group.get("hooks"))
        .and_then(Value::as_array)
        .is_some_and(|handlers| handlers.iter().any(value_hook_handler_is_codey_owned))
}

fn hook_handlers_item_is_codey_owned(handlers: &Item) -> bool {
    match handlers {
        Item::ArrayOfTables(handlers) => handlers.iter().any(table_hook_handler_is_codey_owned),
        Item::Value(Value::Array(handlers)) => {
            handlers.iter().any(value_hook_handler_is_codey_owned)
        }
        _ => false,
    }
}

fn table_hook_handler_is_codey_owned(handler: &Table) -> bool {
    ["command", "commandWindows", "command_windows"]
        .into_iter()
        .filter_map(|key| handler.get(key).and_then(Item::as_str))
        .any(hook_command_is_codey_owned)
}

fn value_hook_handler_is_codey_owned(handler: &Value) -> bool {
    handler.as_inline_table().is_some_and(|handler| {
        ["command", "commandWindows", "command_windows"]
            .into_iter()
            .filter_map(|key| handler.get(key).and_then(Value::as_str))
            .any(hook_command_is_codey_owned)
    })
}

fn hook_command_is_codey_owned(command: &str) -> bool {
    command.contains(crate::subagent_gate::HOOK_ARGUMENT)
        || command.contains(crate::fastctx_route_gate::HOOK_ARGUMENT)
}

fn remap_hook_state_entries(
    state: &mut Table,
    config_path: &Path,
    event_key: &str,
    index_map: &[Option<usize>],
) {
    let prefix = format!("{}:{event_key}:", config_path.display());
    let keys = state
        .iter()
        .filter(|(key, _)| key.starts_with(&prefix))
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    let mut retained = Vec::new();
    for key in keys {
        let Some((group_index, handler_index)) = key[prefix.len()..].split_once(':') else {
            continue;
        };
        let Ok(group_index) = group_index.parse::<usize>() else {
            continue;
        };
        let Some(new_group_index) = index_map.get(group_index) else {
            continue;
        };
        let Some(entry) = state.remove(&key) else {
            continue;
        };
        if let Some(new_group_index) = new_group_index {
            retained.push((format!("{prefix}{new_group_index}:{handler_index}"), entry));
        }
    }
    for (key, entry) in retained {
        state.insert(&key, entry);
    }
}

fn enable_codey_hooks(
    doc: &mut DocumentMut,
    config_path: &Path,
    specs: &[CodeyHookSpec],
    commands: &crate::subagent_gate::HookCommands,
) -> Result<()> {
    let selected_command = if cfg!(windows) {
        commands.command_windows.as_str()
    } else {
        commands.command.as_str()
    };

    for &spec in specs {
        let group_index = {
            let hooks = ensure_root_table(doc, "hooks")?;
            append_codey_hook(hooks, spec, commands)?
        };
        let key = format!(
            "{}:{}:{group_index}:0",
            config_path.display(),
            spec.event_key
        );
        let trusted_hash = crate::subagent_gate::hook_trust_hash(
            spec.event_key,
            spec.matcher,
            selected_command,
            spec.timeout_seconds,
        );
        let hooks = ensure_root_table(doc, "hooks")?;
        let state = ensure_child_table(hooks, "state")?;
        let mut entry = Table::new();
        entry["trusted_hash"] = value(trusted_hash);
        state.insert(&key, Item::Table(entry));
    }
    Ok(())
}

fn append_codey_hook(
    hooks: &mut Table,
    spec: CodeyHookSpec,
    commands: &crate::subagent_gate::HookCommands,
) -> Result<usize> {
    if hooks.get(spec.toml_event).is_none() {
        hooks.insert(spec.toml_event, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let event = hooks
        .get_mut(spec.toml_event)
        .expect("Codey hook event was initialized");
    match event {
        Item::ArrayOfTables(groups) => {
            if let Some(index) = groups
                .iter()
                .position(|group| table_has_hook_definition(group, spec, commands))
            {
                return Ok(index);
            }
            let index = groups.len();
            let mut group = Table::new();
            if let Some(matcher) = spec.matcher {
                group["matcher"] = value(matcher);
            }
            let mut handlers = ArrayOfTables::new();
            handlers.push(codey_hook_table(spec, commands));
            group["hooks"] = Item::ArrayOfTables(handlers);
            groups.push(group);
            Ok(index)
        }
        Item::Value(Value::Array(groups)) => {
            if let Some(index) = groups
                .iter()
                .position(|group| value_has_hook_definition(group, spec, commands))
            {
                return Ok(index);
            }
            let index = groups.len();
            let mut group = InlineTable::new();
            if let Some(matcher) = spec.matcher {
                group.insert("matcher", Value::from(matcher));
            }
            let mut handlers = Array::new();
            handlers.push(Value::InlineTable(codey_hook_inline_table(spec, commands)));
            group.insert("hooks", Value::Array(handlers));
            groups.push(Value::InlineTable(group));
            Ok(index)
        }
        _ => bail!("hooks.{} 必须是 Hook 配置数组", spec.toml_event),
    }
}

fn table_has_hook_definition(
    group: &Table,
    spec: CodeyHookSpec,
    commands: &crate::subagent_gate::HookCommands,
) -> bool {
    group.get("matcher").and_then(Item::as_str) == spec.matcher
        && group
            .get("hooks")
            .and_then(Item::as_array_of_tables)
            .is_some_and(|handlers| {
                handlers.iter().any(|handler| {
                    handler.get("command").and_then(Item::as_str) == Some(commands.command.as_str())
                        && handler.get("commandWindows").and_then(Item::as_str)
                            == Some(commands.command_windows.as_str())
                        && handler.get("timeout").and_then(Item::as_integer)
                            == i64::try_from(spec.timeout_seconds).ok()
                })
            })
}

fn value_has_hook_definition(
    group: &Value,
    spec: CodeyHookSpec,
    commands: &crate::subagent_gate::HookCommands,
) -> bool {
    let Some(group) = group.as_inline_table() else {
        return false;
    };
    group.get("matcher").and_then(Value::as_str) == spec.matcher
        && group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|handlers| {
                handlers.iter().any(|handler| {
                    handler.as_inline_table().is_some_and(|handler| {
                        handler.get("command").and_then(Value::as_str)
                            == Some(commands.command.as_str())
                            && handler.get("commandWindows").and_then(Value::as_str)
                                == Some(commands.command_windows.as_str())
                            && handler.get("timeout").and_then(Value::as_integer)
                                == i64::try_from(spec.timeout_seconds).ok()
                    })
                })
            })
}

fn codey_hook_table(spec: CodeyHookSpec, commands: &crate::subagent_gate::HookCommands) -> Table {
    let mut handler = Table::new();
    handler["type"] = value("command");
    handler["command"] = value(&commands.command);
    handler["commandWindows"] = value(&commands.command_windows);
    handler["timeout"] = value(spec.timeout_seconds as i64);
    handler
}

fn codey_hook_inline_table(
    spec: CodeyHookSpec,
    commands: &crate::subagent_gate::HookCommands,
) -> InlineTable {
    let mut handler = InlineTable::new();
    handler.insert("type", Value::from("command"));
    handler.insert("command", Value::from(commands.command.as_str()));
    handler.insert(
        "commandWindows",
        Value::from(commands.command_windows.as_str()),
    );
    handler.insert("timeout", Value::from(spec.timeout_seconds as i64));
    handler
}

fn local_router_provider_table(endpoint: &RuntimeRouterEndpoint) -> Table {
    let mut provider = Table::new();
    provider["name"] = value(if endpoint.supports_remote_compaction {
        // Codex derives remote compaction from this exact name. Authentication
        // remains an independent provider field so mixed Chat/Responses
        // runtimes can keep official login without advertising compaction to
        // routes that cannot carry its Responses V2 trigger.
        OPENAI_PROVIDER_NAME
    } else {
        LOCAL_ROUTER_PROVIDER_NAME
    });
    provider["base_url"] = value(endpoint.base_url.trim_end_matches('/'));
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(endpoint.requires_openai_auth);
    provider["supports_websockets"] = value(endpoint.supports_websockets);
    let mut headers = InlineTable::new();
    headers.insert(
        local_router::ROUTER_AUTH_HEADER,
        Value::from(endpoint.token.as_str()),
    );
    provider["http_headers"] = Item::Value(Value::InlineTable(headers));
    if !endpoint.requires_openai_auth {
        provider["experimental_bearer_token"] = value(endpoint.token.as_str());
    }
    provider
}

fn update_model_catalog_reference(
    document: &mut DocumentMut,
    config_path: &Path,
    desired_codey_catalog: Option<&Path>,
) {
    let existing = document
        .get("model_catalog_json")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let existing_is_codey_owned =
        existing.is_some_and(|path| is_codey_owned_model_catalog_path(path, config_path));
    match (desired_codey_catalog, existing) {
        (Some(path), None) => {
            document["model_catalog_json"] = value(path.to_string_lossy().into_owned());
        }
        (Some(path), Some(_)) if existing_is_codey_owned => {
            document["model_catalog_json"] = value(path.to_string_lossy().into_owned());
        }
        (None, Some(_)) if existing_is_codey_owned => {
            document.as_table_mut().remove("model_catalog_json");
        }
        _ => {}
    }
}

fn is_codey_owned_model_catalog_path(path: &str, config_path: &Path) -> bool {
    let candidate = Path::new(path);
    let relative = Path::new(crate::model_catalog::relative_path());
    if candidate == relative {
        return true;
    }
    config_path
        .parent()
        .is_some_and(|parent| candidate == parent.join(relative))
}

fn parse_document(existing: &str) -> Result<DocumentMut> {
    if existing.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        existing
            .parse::<DocumentMut>()
            .context("Codex config.toml TOML 解析失败")
    }
}

fn ensure_provider_table(doc: &mut DocumentMut) -> Result<()> {
    if doc
        .get("model_providers")
        .and_then(Item::as_table)
        .is_none()
    {
        doc["model_providers"] = Item::Table(Table::new());
    }
    doc["model_providers"]
        .as_table_mut()
        .map(|_| ())
        .ok_or_else(|| anyhow::anyhow!("model_providers 必须是 TOML table"))
}

fn ensure_root_table<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut Table> {
    if doc.get(key).is_none() {
        doc[key] = Item::Table(Table::new());
    }
    doc[key]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("{key} 必须是 TOML table"))
}

fn document_string(doc: &DocumentMut) -> Result<String> {
    let mut result = doc.to_string();
    if !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

fn enable_desktop_reasoning_efforts(doc: &mut DocumentMut) -> Result<()> {
    if doc.get("desktop").and_then(Item::as_table).is_none() {
        doc["desktop"] = Item::Table(Table::new());
    }
    let desktop = doc["desktop"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("desktop 必须是 TOML table"))?;
    let mut efforts = Array::new();
    for effort in ["low", "medium", "high", "xhigh", "max", "ultra"] {
        efforts.push(effort);
    }
    desktop["enabled-reasoning-efforts"] = value(efforts);
    Ok(())
}

fn ensure_default_service_tier(doc: &mut DocumentMut) {
    if doc.get("service_tier").is_none() {
        doc["service_tier"] = value("default");
    }
}

fn remove_model_selection(doc: &mut DocumentMut) {
    doc.as_table_mut().remove("model");
    let Some(profiles) = doc.get_mut("profiles").and_then(Item::as_table_mut) else {
        return;
    };
    for (_, profile) in profiles.iter_mut() {
        if let Some(profile) = profile.as_table_mut() {
            profile.remove("model");
        }
    }
}

fn set_model_selection(doc: &mut DocumentMut, default_model: Option<&str>) {
    remove_model_selection(doc);
    let Some(default_model) = default_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return;
    };
    doc["model"] = value(default_model);
}

#[cfg(test)]
fn root_key_string(contents: &str, key: &str) -> Option<String> {
    let doc = contents.parse::<DocumentMut>().ok()?;
    doc.get(key).and_then(Item::as_str).map(ToString::to_string)
}

const BACKUP_RETENTION_COUNT: usize = 5;

/// Best-effort retention for the launch backup root: keeps the newest few
/// `{timestamp}-{pid}` run directories plus any directory a live lease still
/// references, so crash recovery always finds its snapshot while stale runs
/// stop accumulating forever.
fn prune_stale_backup_dirs(backup_root: &Path, marker: &Path) {
    let protected = fs::read_to_string(marker)
        .ok()
        .and_then(|contents| serde_json::from_str::<RuntimeConfigLease>(&contents).ok())
        .map(|lease| lease.backup_dir);
    let Ok(entries) = fs::read_dir(backup_root) else {
        return;
    };
    let mut runs = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let name = entry.file_name();
            let (timestamp, pid) = name.to_str()?.split_once('-')?;
            if timestamp.is_empty()
                || pid.is_empty()
                || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
                || !pid.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            let path = entry.path();
            if protected.as_deref() == Some(path.as_path()) {
                return None;
            }
            Some((timestamp.parse::<u128>().ok()?, path))
        })
        .collect::<Vec<_>>();
    if runs.len() <= BACKUP_RETENTION_COUNT {
        return;
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.0));
    for (_, path) in runs.drain(BACKUP_RETENTION_COUNT..) {
        let _ = fs::remove_dir_all(&path);
    }
}

#[cfg(test)]
mod tests;
