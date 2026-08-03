use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use codey_runtime_core::settings::RelayProtocol;
use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, Item, Table, Value, value};

use crate::config::{
    DEFAULT_SUBAGENT_MODEL, DEFAULT_SUBAGENT_REASONING_EFFORT, ProviderProfile,
    SUBAGENT_REASONING_EFFORTS, default_config_path,
};
use crate::fs_util::timestamp_millis;
use crate::provider_lease::CODEY_PROVIDER_ID;

pub const GLOBAL_PROVIDER_ID: &str = "codey_global";
pub const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const OPENAI_PROVIDER_NAME: &str = "OpenAI";
const LEGACY_CODEY_GLOBAL_PROVIDER_NAME: &str = "OpenAI (Codey Global)";
const CODEY_FASTCTX_SERVER_ID: &str = "codey_fastctx";
const CODEY_FASTCTX_NAMESPACE: &str = "mcp__codey_fastctx";
const CODEY_FASTCTX_TOKEN_BUDGET: &str = "8500";
const CODEY_FASTCTX_STARTUP_TIMEOUT_SECONDS: i64 = 15;
const APPLIED_CONFIG_FILE: &str = "applied-config.toml";
const APPLIED_AGENTS_MD_FILE: &str = "applied-AGENTS.md";
const APPLIED_DEFAULT_AGENT_FILE: &str = "agents/applied-default.toml";
const SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

子代理在我们的工作里用于探索，他是你的探子。
把子代理当成你手边最顺手的、用于「宽而重」读取的工具。工作的任何时候，只要你觉得需要就可以派。只有在它能减少主线程上下文污染、提高并行度或者提供独立核验的时候才使用。
必须遵守：你需要更激进和更频繁地调用子代理，在任何需要的情况下，而不仅仅只是在对话的开头。我们需要更频繁的子代理调用来避免上下文腐烂，你承担子代理编排者的角色。

### 何时直接处理

直接读取以及处理以下内容，不派子代理：

- 已知位置的小文件、少量代码或者单一事实；
- 即将修改的具体代码；
- 派发、等待以及复核的成本不低于自己读取的任务。
- 奠基性文档，无论多长都自己读：架构文档、设计文档、交接备忘录（在别的工作流里可能是别的名字）等用来让你建立全局视角、充当后续判断地基的文件——它们的价值全在细节与脉络，一经子代理转译即失真，长度不构成外包的理由。

### 何时适合派发

适合交给子代理的：

- 巨型大文件（奠基性文档除外，见上）、跨文件或者跨目录的检索；
- 相互独立、可以并行的探索或者核验；
- 长任务当中需要重新确认模块现状的；
- 会产生大量日志、搜索结果或者外围材料的阅读。

多个独立的任务应当并发派发。

### 委派与验证

给子代理的任务必须是自包含的，说明检索范围、具体问题以及期望的输出。精度重要的时候，要求返回 `file:line`、符号名以及必要的关键原文——这些出处就是你之后廉价复核的抓手。

子代理的结果只是线索，可能遗漏或者出错。但复核不是把它读过的东西重读一遍，那样这次派发就白费了——你买的是「压缩」，重读会把压缩当场退光。复核 = 顺着它给的 `file:line` 以及关键原文来。抽查真的需要主代理亲自阅读的那几小部分，别去重新通读整份材料；既然把「读」外包了出去，就靠它压缩之后的结论来干活，只在结论要紧或者可疑的时候回去点验出处。

唯二需要你亲自完整读原文的是：① 即将修改的确切代码，② 奠基性文档——这两类本就不外包（见「何时直接处理」）。对它们，子代理至多帮你定位，读由你亲自来：定位与阅读是分工，并非重复劳动。

子代理默认只做探索、检索以及核验。代码修改、方案取舍以及最终验证由主代理来负责。

### 派发机制

- 是否派、派几个由主代理自主决定，无需用户明确要求；较重的探索应当拆成多个独立的轻任务来并发派发。
- 我们系统允许最大并行7个会话进程。所以你最多可以并行分派 6 个子代理；子代理模型的成本较低，无需去顾虑并行派发的成本，只要任务需要就积极使用。
- 子代理一律使用默认配置：工具支持角色参数的时候显式指定 `agent_role = "default"` 或者 `agent_type = "default"`；不支持的时候省略角色、由泛型派生加载 `default.toml`。禁用 `explorer`、`worker` 或者其他角色。
- 派生的时候**必须**显式 `fork_turns = "none"`，不复制主代理的历史，让每个探子都保持干净、快、不背主代理正在腐烂的上下文（代价即上文「任务必须自包含」）。
- 需要多个子代理的时候在同一轮并发派发；派发之后主代理立即 `wait_agent`，停止其余的分析、检索、命令执行以及文件修改，直至全部返回。
- 收到某个子代理结果之后，如果提供了 `close_agent` 就必须立即关闭；每个子代理只用一轮，不复用、不追派。
- 特别注意：子代理自派生起累计运行 10 分钟仍未完成：视为异常，主代理必须介入、不得继续盲等；检查代理状态或运行记录，已有可用 MESSAGE 时采用其部分结果，然后停止这个子代理。并自行判断是否需要再派生或拆分更小任务重新分派。"#;
const DEFAULT_AGENT_CONFIG: &str = r#####"name = "default"

description = "General-purpose exploration subagent using the configured default model and reasoning effort."

developer_instructions = """
你是通用子代理，是主代理派出去的探子。你只做探索、检索、核验：不改动任何东西，不做方案取舍或者最终判断——那些是主代理的事。
不要派生、调用或者请求新的子代理；任务若是需要进一步拆分，把拆分的建议返回给主代理。

你交回给主代理的东西：
- 你的产出直接喂给主代理、是它据以行动的数据，并非给人看的。密而不水，不寒暄、不复述过程、不下客套结论。
- 给证据，不给包装：关键处附上 `file:line`、符号名、必要的逐字原文。主代理会靠这些出处来抽查你、省去重读原文，所以出处必须准、且足以让它核验。
- 把「看到的事实」以及「你的推断」分开，存疑的明确标注——别把猜测写成事实。
- 压缩体量，但承重的精确信息（确切的名字、签名、取值、路径）一字不改地留住，别在转述里磨没了。

你怎么工作：
- 你只有一轮、任务是自包含的：没有追问的机会，别反问；用这一轮把任务范围查到位、尽力答全。
- 答不全就如实交代「查到了什么、还有什么没覆盖、哪里存疑或者矛盾」。宁可显式报「没查到 / 没覆盖」，也别用含糊的话糊弄过去——你悄悄漏掉的，主代理无从复核。
"""

[features]
image_generation = false
"#####;
const CODEY_FASTCTX_GUIDANCE: &str = "Codey FastCtx context tools are enabled. For local file \
reading, content search, and file discovery, always use `mcp__codey_fastctx__read`, \
`mcp__codey_fastctx__grep`, and `mcp__codey_fastctx__glob` before exec or shell commands. \
Do not use cat, sed, rg, grep, find, or recursive ls when a FastCtx tool covers the operation. \
Use exec only for builds, tests, Git, package managers, or when the FastCtx tool is unavailable \
or fails. Use `mcp__codey_fastctx__replace` only for deterministic mechanical replacements, \
and follow every Complete or Partial continuation exactly.";
const LEGACY_CODEY_FASTCTX_GUIDANCE: &str = "Codey FastCtx context tools are enabled. Prefer \
`mcp__codey_fastctx__read`, `mcp__codey_fastctx__grep`, and \
`mcp__codey_fastctx__glob` over shell commands for local file inspection. Use \
`mcp__codey_fastctx__replace` only for deterministic batch replacements, and \
follow every Complete or Partial pagination note exactly.";
const RESERVED_PROVIDER_IDS: [&str; 6] = [
    "amazon-bedrock",
    "openai",
    "ollama",
    "lmstudio",
    "oss",
    "ollama-chat",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeConfigLease {
    backup_dir: PathBuf,
    #[serde(default)]
    config_snapshot_dir: Option<PathBuf>,
    original_config_exists: bool,
    #[serde(default)]
    preserve_provider_route: bool,
    #[serde(default)]
    fastctx_command: Option<PathBuf>,
    #[serde(default)]
    subagent_optimization_applied: bool,
    #[serde(default)]
    subagent_model: String,
    #[serde(default)]
    subagent_reasoning_effort: String,
    #[serde(default)]
    original_agents_md_exists: bool,
    #[serde(default)]
    original_default_agent_exists: bool,
    #[serde(default)]
    original_agents_dir_exists: bool,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    applied_base_url: Option<String>,
}

pub fn codex_home() -> PathBuf {
    codey_runtime_core::relay_config::default_codex_home_dir()
}

fn lease_marker_path() -> PathBuf {
    default_config_path()
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join("codex-lease.json")
}

#[allow(clippy::too_many_arguments)]
pub fn apply_runtime_provider_config(
    home: &Path,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
    default_model: Option<&str>,
    fast_context_tools: bool,
    subagent_optimization: bool,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
) -> Result<PathBuf> {
    let marker = lease_marker_path();
    let backup_root = marker
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join("codex-backups");
    let fastctx_command = fast_context_tools
        .then(std::env::current_exe)
        .transpose()
        .context("定位 Codey 内嵌 FastCtx 服务失败")?;
    apply_runtime_provider_config_at_mode(
        home,
        profile,
        provider_id,
        use_official_catalog,
        default_model,
        fastctx_command.as_deref(),
        subagent_optimization,
        subagent_model,
        subagent_reasoning_effort,
        &marker,
        &backup_root,
        false,
    )
}

pub fn apply_runtime_provider_config_preserving_route(
    home: &Path,
    profile: &ProviderProfile,
    provider_id: &str,
    fast_context_tools: bool,
    subagent_optimization: bool,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
) -> Result<PathBuf> {
    let marker = lease_marker_path();
    let backup_root = marker
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join("codex-backups");
    let fastctx_command = fast_context_tools
        .then(std::env::current_exe)
        .transpose()
        .context("定位 Codey 内嵌 FastCtx 服务失败")?;
    apply_runtime_provider_config_at_mode(
        home,
        profile,
        provider_id,
        false,
        None,
        fastctx_command.as_deref(),
        subagent_optimization,
        subagent_model,
        subagent_reasoning_effort,
        &marker,
        &backup_root,
        true,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn apply_runtime_provider_config_at(
    home: &Path,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
    default_model: Option<&str>,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
    marker: &Path,
    backup_root: &Path,
) -> Result<PathBuf> {
    apply_runtime_provider_config_at_mode(
        home,
        profile,
        provider_id,
        use_official_catalog,
        default_model,
        fastctx_command,
        subagent_optimization,
        DEFAULT_SUBAGENT_MODEL,
        DEFAULT_SUBAGENT_REASONING_EFFORT,
        marker,
        backup_root,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_runtime_provider_config_at_mode(
    home: &Path,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
    default_model: Option<&str>,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
    marker: &Path,
    backup_root: &Path,
    preserve_provider_route: bool,
) -> Result<PathBuf> {
    if !preserve_provider_route {
        ensure_supported_provider_protocol(profile.protocol)?;
    }
    fs::create_dir_all(home)?;
    let config_path = home.join("config.toml");
    let agents_md_path = home.join("AGENTS.md");
    let agents_dir = home.join("agents");
    let default_agent_path = agents_dir.join("default.toml");
    let original_config = read_optional(&config_path)?;
    let original_agents_md = if subagent_optimization {
        read_optional(&agents_md_path)?
    } else {
        None
    };
    let original_default_agent = if subagent_optimization {
        read_optional(&default_agent_path)?
    } else {
        None
    };
    let original_agents_dir_exists = agents_dir.is_dir();
    create_private_dir_all(backup_root)?;
    prune_stale_backup_dirs(backup_root, marker);
    let backup_dir = backup_root.join(format!("{}-{}", timestamp_millis(), std::process::id()));
    create_private_dir_all(&backup_dir)?;
    if let Some(bytes) = original_config.as_deref() {
        write_private_file(&backup_dir.join("config.toml"), bytes)?;
    }

    let existing = str::from_utf8(original_config.as_deref().unwrap_or_default())
        .context("Codex config.toml 不是 UTF-8")?;
    let updated_agents_md = if subagent_optimization {
        let existing_agents_md = str::from_utf8(original_agents_md.as_deref().unwrap_or_default())
            .context("Codex AGENTS.md 不是 UTF-8")?;
        Some(append_subagent_guidance(existing_agents_md))
    } else {
        None
    };
    let provider_id = if preserve_provider_route {
        provider_id.trim().to_string()
    } else {
        normalized_provider_id(provider_id)
    };
    // Codex resolves this path from the app-server working directory, which is
    // `/` for the packaged macOS app, rather than from CODEX_HOME.
    let model_catalog_path =
        use_official_catalog.then(|| home.join(crate::model_catalog::relative_path()));
    let updated = patch_config_with_fastctx_mode(
        existing,
        profile,
        &provider_id,
        model_catalog_path.as_deref(),
        default_model,
        fastctx_command,
        subagent_optimization,
        subagent_model,
        subagent_reasoning_effort,
        preserve_provider_route,
    )?;
    let applied_base_url = provider_base_url(&updated, &provider_id);
    if let Err(error) =
        write_private_file(&backup_dir.join(APPLIED_CONFIG_FILE), updated.as_bytes())
    {
        let _ = fs::remove_dir_all(&backup_dir);
        return Err(error).context("保存 Codey 已应用配置快照失败");
    }
    if subagent_optimization {
        if let Some(bytes) = original_agents_md.as_deref() {
            write_private_file(&backup_dir.join("AGENTS.md"), bytes)?;
        }
        create_private_dir_all(&backup_dir.join("agents"))?;
        if let Some(bytes) = original_default_agent.as_deref() {
            write_private_file(&backup_dir.join("agents/default.toml"), bytes)?;
        }
        write_private_file(
            &backup_dir.join(APPLIED_AGENTS_MD_FILE),
            updated_agents_md
                .as_deref()
                .expect("subagent guidance was prepared")
                .as_bytes(),
        )?;
        write_private_file(
            &backup_dir.join(APPLIED_DEFAULT_AGENT_FILE),
            DEFAULT_AGENT_CONFIG.as_bytes(),
        )?;
    }
    let state = RuntimeConfigLease {
        backup_dir: backup_dir.clone(),
        config_snapshot_dir: None,
        original_config_exists: original_config.is_some(),
        preserve_provider_route,
        fastctx_command: fastctx_command.map(Path::to_path_buf),
        subagent_optimization_applied: subagent_optimization,
        subagent_model: subagent_model.to_string(),
        subagent_reasoning_effort: subagent_reasoning_effort.to_string(),
        original_agents_md_exists: original_agents_md.is_some(),
        original_default_agent_exists: original_default_agent.is_some(),
        original_agents_dir_exists,
        provider_id: Some(provider_id),
        applied_base_url,
    };
    if let Err(error) = write_lease(marker, &state) {
        let _ = fs::remove_dir_all(&backup_dir);
        return Err(error);
    }

    let write_result = (|| -> Result<()> {
        atomic_write(&config_path, updated.as_bytes())?;
        if let Some(updated_agents_md) = updated_agents_md.as_deref() {
            atomic_write(&agents_md_path, updated_agents_md.as_bytes())?;
            create_private_dir_all(&agents_dir)?;
            atomic_write(&default_agent_path, DEFAULT_AGENT_CONFIG.as_bytes())?;
        }
        Ok(())
    })();
    if let Err(write_error) = write_result {
        let mut rollback_results = vec![restore_optional_bytes(
            &config_path,
            original_config.as_deref(),
        )];
        if subagent_optimization {
            rollback_results.push(restore_optional_bytes(
                &agents_md_path,
                original_agents_md.as_deref(),
            ));
            rollback_results.push(restore_optional_bytes(
                &default_agent_path,
                original_default_agent.as_deref(),
            ));
        }
        let rollback_errors = rollback_results
            .into_iter()
            .filter_map(Result::err)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if rollback_errors.is_empty() {
            if subagent_optimization && !original_agents_dir_exists {
                remove_empty_dir(&agents_dir)?;
            }
            let _ = remove_optional(marker);
            let _ = fs::remove_dir_all(&backup_dir);
            return Err(write_error);
        }
        anyhow::bail!(
            "写入 Codey 临时 Codex 配置失败：{write_error}；回滚原配置也失败：{}",
            rollback_errors.join("；")
        );
    }
    Ok(backup_dir)
}

fn append_subagent_guidance(existing: &str) -> String {
    if existing.contains(SUBAGENT_GUIDANCE) {
        return existing.to_string();
    }
    let mut updated = existing.trim_end().to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(SUBAGENT_GUIDANCE);
    updated.push('\n');
    updated
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

pub fn mark_runtime_subagent_defaults_applied(
    home: &Path,
    model: &str,
    reasoning_effort: &str,
) -> Result<()> {
    mark_runtime_subagent_defaults_applied_at(home, &lease_marker_path(), model, reasoning_effort)
}

fn mark_runtime_subagent_defaults_applied_at(
    home: &Path,
    marker: &Path,
    model: &str,
    reasoning_effort: &str,
) -> Result<()> {
    let model = model.trim();
    let reasoning_effort = reasoning_effort.trim().to_ascii_lowercase();
    anyhow::ensure!(!model.is_empty(), "子代理模型不能为空");
    anyhow::ensure!(
        SUBAGENT_REASONING_EFFORTS.contains(&reasoning_effort.as_str()),
        "子代理思考深度无效：{reasoning_effort}"
    );

    let mut state = fs::read_to_string(marker)
        .with_context(|| format!("读取 Codey Codex lease 失败：{}", marker.display()))
        .and_then(|contents| {
            serde_json::from_str::<RuntimeConfigLease>(&contents)
                .with_context(|| format!("解析 Codey Codex lease 失败：{}", marker.display()))
        })?;
    anyhow::ensure!(
        state.subagent_optimization_applied,
        "当前 Codey 运行时未启用子代理协作优化"
    );

    let config_path = home.join("config.toml");
    let current_bytes = fs::read(&config_path)
        .with_context(|| format!("读取 Codex 配置失败：{}", config_path.display()))?;
    let current =
        String::from_utf8(current_bytes.clone()).context("Codex config.toml 不是 UTF-8")?;
    let current_doc = current
        .parse::<DocumentMut>()
        .context("解析 Codex config.toml 失败")?;
    let current_agents = current_doc
        .get("agents")
        .and_then(Item::as_table)
        .context("Codex config.toml 缺少 [agents] 配置")?;
    anyhow::ensure!(
        current_agents
            .get("default_subagent_model")
            .and_then(Item::as_str)
            == Some(model)
            && current_agents
                .get("default_subagent_reasoning_effort")
                .and_then(Item::as_str)
                == Some(reasoning_effort.as_str()),
        "Codex config.toml 尚未写入新的子代理默认配置"
    );

    let snapshot_dir = state
        .config_snapshot_dir
        .as_deref()
        .unwrap_or(&state.backup_dir);
    let applied_path = snapshot_dir.join(APPLIED_CONFIG_FILE);
    let applied = fs::read_to_string(&applied_path)
        .with_context(|| format!("读取 Codey 已应用配置快照失败：{}", applied_path.display()))?;
    let mut applied_doc = applied
        .parse::<DocumentMut>()
        .context("解析 Codey 已应用配置快照失败")?;
    let agents = ensure_root_table(&mut applied_doc, "agents")?;
    agents["default_subagent_model"] = value(model);
    agents["default_subagent_reasoning_effort"] = value(&reasoning_effort);
    let updated_applied = document_string(&applied_doc)?;

    anyhow::ensure!(
        read_optional(&config_path)?.as_deref() == Some(current_bytes.as_slice()),
        "Codex config.toml 在 Codey 更新租约快照前再次变化"
    );
    atomic_write(&applied_path, updated_applied.as_bytes())?;
    state.subagent_model = model.to_string();
    state.subagent_reasoning_effort = reasoning_effort;
    write_lease(marker, &state)
}

pub fn reconcile_runtime_config_overlay(home: &Path) -> Result<Option<Vec<u8>>> {
    reconcile_runtime_config_overlay_at(home, &lease_marker_path())
}

fn reconcile_runtime_config_overlay_at(home: &Path, marker: &Path) -> Result<Option<Vec<u8>>> {
    let mut state = match fs::read_to_string(marker) {
        Ok(contents) => serde_json::from_str::<RuntimeConfigLease>(&contents)
            .with_context(|| format!("解析 Codey Codex lease 失败：{}", marker.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !state.preserve_provider_route {
        return Ok(None);
    }

    let config_path = home.join("config.toml");
    let Some(current_bytes) = read_optional(&config_path)? else {
        // CC Switch writes its Live file independently. Do not recreate a file
        // that may be between replacement steps; the watcher will retry.
        return Ok(None);
    };
    let current =
        String::from_utf8(current_bytes.clone()).context("Codex config.toml 不是 UTF-8")?;
    let snapshot_dir = state
        .config_snapshot_dir
        .as_deref()
        .unwrap_or(&state.backup_dir);
    let applied_path = snapshot_dir.join(APPLIED_CONFIG_FILE);
    let applied_bytes = fs::read(&applied_path)
        .with_context(|| format!("读取 Codey 已应用配置快照失败：{}", applied_path.display()))?;
    if current_bytes == applied_bytes {
        return Ok(Some(current_bytes));
    }

    let original = if state.original_config_exists {
        let original_path = snapshot_dir.join("config.toml");
        fs::read_to_string(&original_path)
            .with_context(|| format!("找不到 Codex 原配置备份：{}", original_path.display()))?
    } else {
        String::new()
    };
    let applied = String::from_utf8(applied_bytes).context("Codey 已应用配置快照不是 UTF-8")?;
    let baseline = restore_owned_config_changes(&original, &applied, &current)
        .context("提取 CC Switch 最新 Live 配置失败")?;
    let updated = patch_config_preserving_provider_route(
        &baseline,
        state.fastctx_command.as_deref(),
        state.subagent_optimization_applied,
        if state.subagent_model.trim().is_empty() {
            DEFAULT_SUBAGENT_MODEL
        } else {
            state.subagent_model.as_str()
        },
        if state.subagent_reasoning_effort.trim().is_empty() {
            DEFAULT_SUBAGENT_REASONING_EFFORT
        } else {
            state.subagent_reasoning_effort.as_str()
        },
    )
    .context("重新应用 Codey 运行时增强失败")?;

    if read_optional(&config_path)?.as_deref() != Some(current_bytes.as_slice()) {
        anyhow::bail!("Codex Live 配置在 Codey 准备重新应用增强时再次变化");
    }

    let snapshots_root = state.backup_dir.join("route-snapshots");
    create_private_dir_all(&snapshots_root)?;
    let next_snapshot_dir =
        snapshots_root.join(format!("{}-{}", timestamp_millis(), std::process::id()));
    create_private_dir_all(&next_snapshot_dir)?;
    write_private_file(&next_snapshot_dir.join("config.toml"), baseline.as_bytes())?;
    write_private_file(
        &next_snapshot_dir.join(APPLIED_CONFIG_FILE),
        updated.as_bytes(),
    )?;

    let previous_snapshot_dir = state.config_snapshot_dir.replace(next_snapshot_dir);
    state.original_config_exists = true;
    state.provider_id = root_key_string(&updated, "model_provider");
    state.applied_base_url = state
        .provider_id
        .as_deref()
        .and_then(|provider_id| provider_base_url(&updated, provider_id));
    write_lease(marker, &state)?;
    if read_optional(&config_path)?.as_deref() != Some(current_bytes.as_slice()) {
        anyhow::bail!("Codex Live 配置在 Codey 保存增强快照后再次变化");
    }
    if updated.as_bytes() != current_bytes {
        atomic_write(&config_path, updated.as_bytes())?;
    }
    // The rolled lease now points at the new snapshot, so the superseded one
    // is unreachable for every recovery path and can be dropped immediately.
    if let Some(previous) = previous_snapshot_dir
        .filter(|previous| Some(previous) != state.config_snapshot_dir.as_ref())
    {
        let _ = fs::remove_dir_all(previous);
    }
    Ok(Some(updated.into_bytes()))
}

pub fn restore_runtime_provider_config(home: &Path) -> Result<bool> {
    restore_runtime_provider_config_at(home, &lease_marker_path())
}

fn restore_runtime_provider_config_at(home: &Path, marker: &Path) -> Result<bool> {
    let state = match fs::read_to_string(marker) {
        Ok(contents) => serde_json::from_str::<RuntimeConfigLease>(&contents)
            .with_context(|| format!("解析 Codey Codex lease 失败：{}", marker.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let config_path = home.join("config.toml");
    let current = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 Codex 配置失败：{}", config_path.display()));
        }
    };
    let provider_id = state.provider_id.as_deref().unwrap_or(CODEY_PROVIDER_ID);
    let provider_matches =
        root_key_string(&current, "model_provider").as_deref() == Some(provider_id);
    let endpoint_matches = state.applied_base_url.as_deref().is_none_or(|base_url| {
        provider_base_url(&current, provider_id).as_deref() == Some(base_url)
    });
    if !state.preserve_provider_route && (!provider_matches || !endpoint_matches) {
        restore_runtime_subagent_files(home, &state)?;
        remove_optional(marker)?;
        return Ok(false);
    }

    let config_snapshot_dir = state
        .config_snapshot_dir
        .as_deref()
        .unwrap_or(&state.backup_dir);
    let backup_config = config_snapshot_dir.join("config.toml");
    let original = if state.original_config_exists {
        fs::read_to_string(&backup_config)
            .with_context(|| format!("找不到 Codex 原配置备份：{}", backup_config.display()))?
    } else {
        String::new()
    };
    let applied_config = config_snapshot_dir.join(APPLIED_CONFIG_FILE);
    let restored = if applied_config.exists() {
        let applied = fs::read_to_string(&applied_config).with_context(|| {
            format!(
                "读取 Codey 已应用配置快照失败：{}",
                applied_config.display()
            )
        })?;
        restore_owned_config_changes(&original, &applied, &current)?
    } else {
        restore_legacy_owned_config_changes(&original, &current, provider_id)?
    };
    if !state.original_config_exists && restored.trim().is_empty() {
        remove_optional(&config_path)?;
    } else {
        atomic_write(&config_path, restored.as_bytes())?;
    }
    restore_runtime_subagent_files(home, &state)?;
    remove_optional(marker)?;
    Ok(true)
}

fn restore_runtime_subagent_files(home: &Path, state: &RuntimeConfigLease) -> Result<()> {
    if !state.subagent_optimization_applied {
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

fn remove_subagent_guidance(current: &str) -> Option<String> {
    let guidance_start = current.find(SUBAGENT_GUIDANCE)?;
    let mut owned_start = guidance_start;
    if current[..owned_start].ends_with("\n\n") {
        owned_start -= 2;
    }
    let mut owned_end = guidance_start + SUBAGENT_GUIDANCE.len();
    if current[owned_end..].starts_with('\n') {
        owned_end += 1;
    }
    let mut restored = current[..owned_start].to_string();
    restored.push_str(&current[owned_end..]);
    Some(restored)
}

fn restore_if_still_applied(path: &Path, original: Option<&[u8]>, applied: &[u8]) -> Result<()> {
    if read_optional(path)?.as_deref() == Some(applied) {
        restore_optional_bytes(path, original)?;
    }
    Ok(())
}

fn restore_legacy_owned_config_changes(
    original: &str,
    current: &str,
    provider_id: &str,
) -> Result<String> {
    let original_document = parse_document(original).context("解析旧版 Codex 原配置备份失败")?;
    let current_document = parse_document(current).context("解析旧版 Codex 当前配置失败")?;
    let mut applied_document =
        parse_document(original).context("准备旧版 Codey 配置恢复基线失败")?;

    if current_document
        .get("model_provider")
        .and_then(Item::as_str)
        == Some(provider_id)
        && let Some(item) = current_document.get("model_provider")
    {
        applied_document
            .as_table_mut()
            .insert("model_provider", item.clone());
    }

    if let Some(current_provider) = current_document
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table)
    {
        let applied_provider = table_with_selected_fields(
            current_provider,
            &[
                "name",
                "base_url",
                "wire_api",
                "requires_openai_auth",
                "experimental_bearer_token",
            ],
        );
        ensure_root_table(&mut applied_document, "model_providers")?
            .insert(provider_id, Item::Table(applied_provider));
    }

    match current_document.get("model_catalog_json") {
        Some(item) if item.as_str() == Some(crate::model_catalog::relative_path()) => {
            applied_document
                .as_table_mut()
                .insert("model_catalog_json", item.clone());
        }
        None if original_document.get("model_catalog_json").is_some() => {
            applied_document.as_table_mut().remove("model_catalog_json");
        }
        _ => {}
    }

    if original_document.get("model").is_some() && current_document.get("model").is_none() {
        applied_document.as_table_mut().remove("model");
    }
    remove_legacy_active_profile_model(
        &original_document,
        &current_document,
        &mut applied_document,
    );

    if let Some(efforts) = current_document
        .get("desktop")
        .and_then(Item::as_table)
        .and_then(|desktop| desktop.get("enabled-reasoning-efforts"))
        .filter(|item| is_legacy_reasoning_efforts(item))
    {
        ensure_root_table(&mut applied_document, "desktop")?
            .insert("enabled-reasoning-efforts", efforts.clone());
    }

    if let Some(server) = current_document
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(CODEY_FASTCTX_SERVER_ID))
        .and_then(Item::as_table)
        .filter(|server| legacy_fastctx_server_is_codey_owned(server))
    {
        let mut applied_server = table_with_selected_fields(
            server,
            &["command", "args", "startup_timeout_sec", "tool_timeout_sec"],
        );
        if let Some(environment) = server.get("env").and_then(Item::as_table) {
            let applied_environment =
                table_with_selected_fields(environment, &["FASTCTX_TOKEN_BUDGET"]);
            if !applied_environment.is_empty() {
                applied_server.insert("env", Item::Table(applied_environment));
            }
        }
        ensure_root_table(&mut applied_document, "mcp_servers")?
            .insert(CODEY_FASTCTX_SERVER_ID, Item::Table(applied_server));
    }

    let original_namespaces = fastctx_namespaces(&original_document);
    let current_namespaces = fastctx_namespaces(&current_document);
    let original_has_fastctx = original_namespaces.is_some_and(|namespaces| {
        namespaces
            .iter()
            .any(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
    });
    let current_has_fastctx = current_namespaces.is_some_and(|namespaces| {
        namespaces
            .iter()
            .any(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
    });
    if !original_has_fastctx && current_has_fastctx {
        let mut applied_namespaces = original_namespaces.cloned().unwrap_or_else(Array::new);
        applied_namespaces.push(CODEY_FASTCTX_NAMESPACE);
        let features = ensure_root_table(&mut applied_document, "features")?;
        let code_mode = ensure_child_table(features, "code_mode")?;
        code_mode.insert(
            "direct_only_tool_namespaces",
            Item::Value(Value::Array(applied_namespaces)),
        );
    }

    if original_document.get("tool_output_token_limit").is_none()
        && current_document
            .get("tool_output_token_limit")
            .and_then(Item::as_integer)
            == Some(10_000)
        && let Some(item) = current_document.get("tool_output_token_limit")
    {
        applied_document
            .as_table_mut()
            .insert("tool_output_token_limit", item.clone());
    }

    let original_guidance = original_document
        .get("developer_instructions")
        .and_then(Item::as_str)
        .unwrap_or_default();
    let current_guidance = current_document
        .get("developer_instructions")
        .and_then(Item::as_str)
        .unwrap_or_default();
    let mut applied_guidance = original_guidance.to_string();
    let mut fastctx_guidance_was_applied = false;
    for guidance in [CODEY_FASTCTX_GUIDANCE, LEGACY_CODEY_FASTCTX_GUIDANCE] {
        if original_guidance.contains(guidance) || !current_guidance.contains(guidance) {
            continue;
        }
        if applied_guidance.trim().is_empty() {
            applied_guidance = guidance.to_string();
        } else {
            applied_guidance.push_str("\n\n");
            applied_guidance.push_str(guidance);
        }
        fastctx_guidance_was_applied = true;
    }
    if fastctx_guidance_was_applied {
        applied_document["developer_instructions"] = value(applied_guidance);
    }

    let applied = document_string(&applied_document)?;
    restore_owned_config_changes(original, &applied, current)
}

fn remove_legacy_active_profile_model(
    original: &DocumentMut,
    current: &DocumentMut,
    applied: &mut DocumentMut,
) {
    let Some(active_profile) = original.get("profile").and_then(Item::as_str) else {
        return;
    };
    let original_has_model = original
        .get("profiles")
        .and_then(Item::as_table)
        .and_then(|profiles| profiles.get(active_profile))
        .and_then(Item::as_table)
        .is_some_and(|profile| profile.get("model").is_some());
    let current_profile = current
        .get("profiles")
        .and_then(Item::as_table)
        .and_then(|profiles| profiles.get(active_profile))
        .and_then(Item::as_table);
    if !original_has_model || current_profile.is_none_or(|profile| profile.get("model").is_some()) {
        return;
    }
    if let Some(applied_profile) = applied
        .get_mut("profiles")
        .and_then(Item::as_table_mut)
        .and_then(|profiles| profiles.get_mut(active_profile))
        .and_then(Item::as_table_mut)
    {
        applied_profile.remove("model");
    }
}

fn table_with_selected_fields(source: &Table, fields: &[&str]) -> Table {
    let mut selected = Table::new();
    for field in fields {
        if let Some(item) = source.get(field) {
            selected.insert(field, item.clone());
        }
    }
    selected
}

fn legacy_fastctx_server_is_codey_owned(server: &Table) -> bool {
    server
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|arguments| {
            arguments
                .iter()
                .any(|argument| argument.as_str() == Some("--codey-fastctx-mcp"))
        })
}

fn fastctx_namespaces(document: &DocumentMut) -> Option<&Array> {
    document
        .get("features")
        .and_then(Item::as_table)
        .and_then(|features| features.get("code_mode"))
        .and_then(Item::as_table)
        .and_then(|code_mode| code_mode.get("direct_only_tool_namespaces"))
        .and_then(Item::as_array)
}

fn is_legacy_reasoning_efforts(item: &Item) -> bool {
    const LEGACY_EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];
    item.as_array().is_some_and(|efforts| {
        efforts.len() == LEGACY_EFFORTS.len()
            && efforts
                .iter()
                .zip(LEGACY_EFFORTS)
                .all(|(actual, expected)| actual.as_str() == Some(expected))
    })
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

fn restore_owned_config_changes(original: &str, applied: &str, current: &str) -> Result<String> {
    let original = parse_document(original).context("解析 Codex 原配置备份失败")?;
    let applied = parse_document(applied).context("解析 Codey 已应用配置快照失败")?;
    let mut current = parse_document(current).context("解析 Codex 当前配置失败")?;
    restore_table_changes(
        original.as_table(),
        applied.as_table(),
        current.as_table_mut(),
    );
    if current.as_table().is_empty() {
        Ok(String::new())
    } else {
        document_string(&current)
    }
}

fn restore_table_changes(original: &Table, applied: &Table, current: &mut Table) {
    let keys = original
        .iter()
        .chain(applied.iter())
        .map(|(key, _)| key.to_string())
        .collect::<BTreeSet<_>>();

    for key in keys {
        let original_item = original.get(&key).filter(|item| !item.is_none());
        let applied_item = applied.get(&key).filter(|item| !item.is_none());
        if optional_items_semantically_equal(original_item, applied_item) {
            continue;
        }

        let current_matches_applied = optional_items_semantically_equal(
            current.get(&key).filter(|item| !item.is_none()),
            applied_item,
        );
        if current_matches_applied {
            if let Some(original_item) = original_item {
                current.insert(&key, original_item.clone());
            } else {
                current.remove(&key);
            }
            continue;
        }

        if key == CODEY_FASTCTX_SERVER_ID && original_item.is_none() {
            let still_codey_owned = applied_item
                .and_then(Item::as_table)
                .zip(current.get(&key).and_then(Item::as_table))
                .is_some_and(|(applied, current)| {
                    ["command", "args"].iter().all(|field| {
                        optional_items_semantically_equal(applied.get(field), current.get(field))
                    })
                });
            if still_codey_owned {
                current.remove(&key);
            }
            // A complete replacement under the reserved id belongs to the
            // concurrent writer; do not strip matching fields out of it.
            continue;
        }

        if restore_fastctx_owned_value(&key, original_item, applied_item, current.get_mut(&key)) {
            continue;
        }

        let empty_original = Table::new();
        let original_table = match original_item {
            Some(item) => item.as_table(),
            None => Some(&empty_original),
        };
        let applied_table = applied_item.and_then(Item::as_table);
        let mut remove_empty_added_table = false;
        if let (Some(original_table), Some(applied_table), Some(current_table)) = (
            original_table,
            applied_table,
            current.get_mut(&key).and_then(Item::as_table_mut),
        ) {
            restore_table_changes(original_table, applied_table, current_table);
            remove_empty_added_table = original_item.is_none() && current_table.is_empty();
        }
        if remove_empty_added_table {
            current.remove(&key);
        }
    }
}

fn restore_fastctx_owned_value(
    key: &str,
    original: Option<&Item>,
    applied: Option<&Item>,
    current: Option<&mut Item>,
) -> bool {
    match key {
        "direct_only_tool_namespaces" => {
            let original_has_namespace = original.and_then(Item::as_array).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
            });
            let applied_has_namespace = applied.and_then(Item::as_array).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
            });
            if original_has_namespace || !applied_has_namespace {
                return false;
            }
            let Some(entries) = current.and_then(Item::as_array_mut) else {
                return false;
            };
            let Some(index) = entries
                .iter()
                .position(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
            else {
                return false;
            };
            entries.remove(index);
            true
        }
        "developer_instructions" => {
            let Some(current) = current else {
                return false;
            };
            let Some(text) = current.as_str() else {
                return false;
            };
            let mut restored = text.to_string();
            let mut changed = false;
            for guidance in [CODEY_FASTCTX_GUIDANCE, LEGACY_CODEY_FASTCTX_GUIDANCE] {
                let original_has_guidance = original
                    .and_then(Item::as_str)
                    .is_some_and(|text| text.contains(guidance));
                let applied_has_guidance = applied
                    .and_then(Item::as_str)
                    .is_some_and(|text| text.contains(guidance));
                if original_has_guidance || !applied_has_guidance {
                    continue;
                }
                while let Some(without_guidance) = remove_owned_guidance_block(&restored, guidance)
                {
                    restored = without_guidance;
                    changed = true;
                }
            }
            if changed {
                *current = value(restored);
            }
            changed
        }
        _ => false,
    }
}

fn optional_items_semantically_equal(left: Option<&Item>, right: Option<&Item>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => items_semantically_equal(left, right),
        _ => false,
    }
}

fn items_semantically_equal(left: &Item, right: &Item) -> bool {
    match (left, right) {
        (Item::None, Item::None) => true,
        (Item::Value(left), Item::Value(right)) => values_semantically_equal(left, right),
        (Item::Table(left), Item::Table(right)) => tables_semantically_equal(left, right),
        (Item::ArrayOfTables(left), Item::ArrayOfTables(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| tables_semantically_equal(left, right))
        }
        _ => false,
    }
}

fn tables_semantically_equal(left: &Table, right: &Table) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, left)| {
            right
                .get(key)
                .is_some_and(|right| items_semantically_equal(left, right))
        })
}

fn values_semantically_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::String(left), Value::String(right)) => left.value() == right.value(),
        (Value::Integer(left), Value::Integer(right)) => left.value() == right.value(),
        (Value::Float(left), Value::Float(right)) => {
            left.value().to_bits() == right.value().to_bits()
        }
        (Value::Boolean(left), Value::Boolean(right)) => left.value() == right.value(),
        (Value::Datetime(left), Value::Datetime(right)) => left.value() == right.value(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| values_semantically_equal(left, right))
        }
        (Value::InlineTable(left), Value::InlineTable(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| values_semantically_equal(left, right))
                })
        }
        _ => false,
    }
}

/// Reads the provider selected by an external Live-route owner without
/// normalizing or rewriting the surrounding Codex configuration.
pub fn active_model_provider(home: &Path) -> Result<String> {
    let config_path = home.join("config.toml");
    let contents = fs::read_to_string(&config_path)
        .with_context(|| format!("读取 Codex 配置失败：{}", config_path.display()))?;
    root_key_string(&contents, "model_provider")
        .filter(|provider| !provider.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("CC Switch 路由配置缺少活动 model_provider"))
}

/// Installs a stable non-reserved provider for the official account flow.
/// Direct third-party profiles temporarily reuse this provider id while Codey
/// runs, then the exact original configuration is restored.
pub fn ensure_global_model_provider(home: &Path) -> Result<String> {
    fs::create_dir_all(home)?;
    let config_path = home.join("config.toml");
    let original = read_optional(&config_path)?;
    let existing = String::from_utf8(original.clone().unwrap_or_default())
        .context("Codex config.toml 不是 UTF-8")?;
    let mut doc = parse_document(&existing)?;

    let current_provider = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(ToString::to_string);
    let current_provider_config = current_provider
        .as_deref()
        .and_then(|provider| {
            doc.get("model_providers")
                .and_then(Item::as_table)
                .and_then(|providers| providers.get(provider))
        })
        .filter(|provider| provider.as_table_like().is_some())
        .cloned();

    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        for provider in RESERVED_PROVIDER_IDS {
            providers.remove(provider);
        }
    }
    if let Some(provider) = current_provider.as_deref()
        && !is_reserved_provider(provider)
        && provider != CODEY_PROVIDER_ID
        && provider != GLOBAL_PROVIDER_ID
    {
        write_global_provider_migration_if_changed(home, &config_path, &existing, &doc, original)?;
        return Ok(provider.to_string());
    }

    ensure_provider_table(&mut doc)?;
    let mut global_provider =
        current_provider_config.unwrap_or_else(|| Item::Table(official_provider_table()));
    migrate_legacy_official_provider_name(&mut global_provider);
    doc["model_providers"]
        .as_table_mut()
        .expect("model_providers was initialized")[GLOBAL_PROVIDER_ID] = global_provider;
    doc["model_provider"] = value(GLOBAL_PROVIDER_ID);
    write_global_provider_migration_if_changed(home, &config_path, &existing, &doc, original)?;
    Ok(GLOBAL_PROVIDER_ID.to_string())
}

#[cfg(test)]
pub fn patch_config(
    existing: &str,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
) -> Result<String> {
    let model_catalog_path =
        use_official_catalog.then(|| Path::new(crate::model_catalog::relative_path()));
    patch_config_with_fastctx(
        existing,
        profile,
        provider_id,
        model_catalog_path,
        None,
        None,
        false,
    )
}

#[cfg(test)]
fn patch_config_with_fastctx(
    existing: &str,
    profile: &ProviderProfile,
    provider_id: &str,
    model_catalog_path: Option<&Path>,
    default_model: Option<&str>,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
) -> Result<String> {
    patch_config_with_fastctx_mode(
        existing,
        profile,
        provider_id,
        model_catalog_path,
        default_model,
        fastctx_command,
        subagent_optimization,
        DEFAULT_SUBAGENT_MODEL,
        DEFAULT_SUBAGENT_REASONING_EFFORT,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn patch_config_with_fastctx_mode(
    existing: &str,
    profile: &ProviderProfile,
    provider_id: &str,
    model_catalog_path: Option<&Path>,
    default_model: Option<&str>,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
    preserve_provider_route: bool,
) -> Result<String> {
    if !preserve_provider_route {
        ensure_supported_provider_protocol(profile.protocol)?;
    }
    let mut doc = parse_document(existing)?;
    if preserve_provider_route {
        ensure_active_provider_uses_responses(&doc)?;
    }
    // CC Switch owns all routing and model-selection fields while Live
    // takeover is active. Codey only layers its independent runtime
    // enhancements onto the current Live document.
    if !preserve_provider_route {
        ensure_provider_table(&mut doc)?;
        let provider_id = normalized_provider_id(provider_id);
        let existing_local_provider = profile
            .cc_switch_provider_id
            .is_none()
            .then(|| {
                doc.get("model_providers")
                    .and_then(Item::as_table)
                    .and_then(|providers| providers.get(&provider_id))
                    .and_then(Item::as_table)
                    .cloned()
            })
            .flatten();
        let provider = if profile.cc_switch_read_only {
            official_provider_table()
        } else {
            direct_provider_table(profile, existing_local_provider)?
        };
        doc["model_providers"]
            .as_table_mut()
            .expect("model_providers was initialized")[&provider_id] = Item::Table(provider);
        doc["model_provider"] = value(provider_id);
        if let Some(model_catalog_path) = model_catalog_path {
            doc["model_catalog_json"] = value(model_catalog_path.to_string_lossy().into_owned());
        } else {
            doc.as_table_mut().remove("model_catalog_json");
        }
        set_model_selection(&mut doc, default_model);
    }
    enable_desktop_reasoning_efforts(&mut doc)?;
    ensure_default_service_tier(&mut doc);
    if let Some(command) = fastctx_command {
        enable_fast_context_tools(&mut doc, command)?;
    } else {
        disable_fast_context_tools(&mut doc);
    }
    if subagent_optimization {
        enable_subagent_optimization(&mut doc, subagent_model, subagent_reasoning_effort)?;
    }
    document_string(&doc)
}

fn patch_config_preserving_provider_route(
    existing: &str,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
) -> Result<String> {
    patch_config_with_fastctx_mode(
        existing,
        &ProviderProfile::new("route-preserve"),
        "",
        None,
        None,
        fastctx_command,
        subagent_optimization,
        subagent_model,
        subagent_reasoning_effort,
        true,
    )
}

fn enable_subagent_optimization(
    doc: &mut DocumentMut,
    subagent_model: &str,
    subagent_reasoning_effort: &str,
) -> Result<()> {
    doc.as_table_mut().remove("agents");
    let agents = ensure_root_table(doc, "agents")?;
    agents["default_subagent_model"] = value(subagent_model.trim());
    agents["default_subagent_reasoning_effort"] =
        value(subagent_reasoning_effort.trim().to_ascii_lowercase());
    let features = ensure_root_table(doc, "features")?;
    if features.get("multi_agent_v2").is_none() {
        features["multi_agent_v2"] = Item::Table(Table::new());
    }
    let multi_agent = features["multi_agent_v2"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("features.multi_agent_v2 必须是 TOML table"))?;
    multi_agent["enabled"] = value(true);
    multi_agent["hide_spawn_agent_metadata"] = value(true);
    multi_agent["tool_namespace"] = value("agents");
    multi_agent["max_concurrent_threads_per_session"] = value(7);
    multi_agent["min_wait_timeout_ms"] = value(10_000);
    multi_agent["default_wait_timeout_ms"] = value(30_000);
    multi_agent["max_wait_timeout_ms"] = value(120_000);
    Ok(())
}

fn enable_fast_context_tools(doc: &mut DocumentMut, command: &Path) -> Result<()> {
    let codey_owned_server = doc
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(CODEY_FASTCTX_SERVER_ID))
        .and_then(Item::as_table)
        .is_some_and(legacy_fastctx_server_is_codey_owned);
    if has_configured_fastctx_server(doc) && !codey_owned_server {
        return Ok(());
    }

    let mcp_servers = ensure_root_table(doc, "mcp_servers")?;
    if !codey_owned_server {
        mcp_servers.insert(CODEY_FASTCTX_SERVER_ID, Item::Table(Table::new()));
    }
    let server = mcp_servers
        .get_mut(CODEY_FASTCTX_SERVER_ID)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| {
            anyhow::anyhow!("mcp_servers.{CODEY_FASTCTX_SERVER_ID} 必须是 TOML table")
        })?;
    server["command"] = value(command.to_string_lossy().to_string());
    let mut args = Array::new();
    args.push("--codey-fastctx-mcp");
    server["args"] = Item::Value(toml_edit::Value::Array(args));
    server["startup_timeout_sec"] = value(CODEY_FASTCTX_STARTUP_TIMEOUT_SECONDS);
    server["tool_timeout_sec"] = value(120);
    let mut env = server
        .get("env")
        .and_then(Item::as_table)
        .cloned()
        .unwrap_or_default();
    env["FASTCTX_TOKEN_BUDGET"] = value(CODEY_FASTCTX_TOKEN_BUDGET);
    server["env"] = Item::Table(env);

    let features = ensure_root_table(doc, "features")?;
    if features.get("code_mode").is_none() {
        features["code_mode"] = Item::Table(Table::new());
    }
    let code_mode = features["code_mode"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("features.code_mode 必须是 TOML table"))?;
    if code_mode.get("direct_only_tool_namespaces").is_none() {
        code_mode["direct_only_tool_namespaces"] =
            Item::Value(toml_edit::Value::Array(Array::new()));
    }
    let namespaces = code_mode["direct_only_tool_namespaces"]
        .as_array_mut()
        .ok_or_else(|| {
            anyhow::anyhow!("features.code_mode.direct_only_tool_namespaces 必须是数组")
        })?;
    if !namespaces
        .iter()
        .any(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
    {
        namespaces.push(CODEY_FASTCTX_NAMESPACE);
    }

    if doc.get("tool_output_token_limit").is_none() {
        doc["tool_output_token_limit"] = value(10_000);
    }
    let existing_guidance = doc
        .get("developer_instructions")
        .map(|item| {
            item.as_str()
                .ok_or_else(|| anyhow::anyhow!("developer_instructions 必须是字符串"))
        })
        .transpose()?
        .unwrap_or_default();
    if !existing_guidance.contains(CODEY_FASTCTX_GUIDANCE) {
        let guidance = if existing_guidance.trim().is_empty() {
            CODEY_FASTCTX_GUIDANCE.to_string()
        } else {
            format!("{existing_guidance}\n\n{CODEY_FASTCTX_GUIDANCE}")
        };
        doc["developer_instructions"] = value(guidance);
    }
    Ok(())
}

fn disable_fast_context_tools(doc: &mut DocumentMut) {
    let codey_owned_server_removed =
        if let Some(mcp_servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_mut) {
            let codey_owned_server = mcp_servers
                .get(CODEY_FASTCTX_SERVER_ID)
                .and_then(Item::as_table)
                .is_some_and(legacy_fastctx_server_is_codey_owned);
            if codey_owned_server {
                mcp_servers.remove(CODEY_FASTCTX_SERVER_ID);
            }
            codey_owned_server
        } else {
            false
        };

    let existing_guidance = doc
        .get("developer_instructions")
        .and_then(Item::as_str)
        .map(ToString::to_string);
    let restored_guidance =
        existing_guidance.and_then(|guidance| remove_codey_fastctx_guidance(&guidance));
    let codey_guidance_removed = restored_guidance.is_some();
    if let Some(restored_guidance) = restored_guidance {
        if restored_guidance.trim().is_empty() {
            doc.as_table_mut().remove("developer_instructions");
        } else {
            doc["developer_instructions"] = value(restored_guidance);
        }
    }

    let reserved_server_remains = doc
        .get("mcp_servers")
        .and_then(Item::as_table)
        .is_some_and(|mcp_servers| mcp_servers.contains_key(CODEY_FASTCTX_SERVER_ID));
    if (codey_owned_server_removed || codey_guidance_removed)
        && !reserved_server_remains
        && let Some(namespaces) = doc
            .get_mut("features")
            .and_then(Item::as_table_mut)
            .and_then(|features| features.get_mut("code_mode"))
            .and_then(Item::as_table_mut)
            .and_then(|code_mode| code_mode.get_mut("direct_only_tool_namespaces"))
            .and_then(Item::as_array_mut)
    {
        loop {
            let index = namespaces
                .iter()
                .position(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE));
            let Some(index) = index else {
                break;
            };
            namespaces.remove(index);
        }
    }
}

fn has_configured_fastctx_server(doc: &DocumentMut) -> bool {
    let Some(mcp_servers) = doc.get("mcp_servers").and_then(Item::as_table) else {
        return false;
    };

    mcp_servers.iter().any(|(server_id, server)| {
        mentions_fastctx(server_id)
            || server.as_table().is_some_and(|server| {
                server
                    .get("command")
                    .and_then(Item::as_str)
                    .is_some_and(mentions_fastctx)
                    || server
                        .get("args")
                        .and_then(Item::as_array)
                        .is_some_and(|arguments| {
                            arguments
                                .iter()
                                .filter_map(toml_edit::Value::as_str)
                                .any(mentions_fastctx)
                        })
            })
            || matches!(
                server,
                Item::Value(Value::InlineTable(server))
                    if server
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(mentions_fastctx)
                        || server
                            .get("args")
                            .and_then(Value::as_array)
                            .is_some_and(|arguments| {
                                arguments
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .any(mentions_fastctx)
                            })
            )
    })
}

fn mentions_fastctx(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case("fastctx"))
}

fn remove_codey_fastctx_guidance(current: &str) -> Option<String> {
    let mut restored = current.to_string();
    let mut changed = false;
    for guidance in [CODEY_FASTCTX_GUIDANCE, LEGACY_CODEY_FASTCTX_GUIDANCE] {
        while let Some(without_guidance) = remove_guidance_paragraph(&restored, guidance) {
            restored = without_guidance;
            changed = true;
        }
    }
    changed.then_some(restored)
}

fn remove_owned_guidance_block(current: &str, guidance: &str) -> Option<String> {
    let guidance_start = current.find(guidance)?;
    Some(remove_guidance_at(current, guidance_start, guidance.len()))
}

fn remove_guidance_paragraph(current: &str, guidance: &str) -> Option<String> {
    let guidance_start = current.match_indices(guidance).find_map(|(start, _)| {
        let end = start + guidance.len();
        let starts_paragraph = start == 0 || current[..start].ends_with("\n\n");
        let ends_paragraph = end == current.len() || current[end..].starts_with("\n\n");
        (starts_paragraph && ends_paragraph).then_some(start)
    })?;
    Some(remove_guidance_at(current, guidance_start, guidance.len()))
}

fn remove_guidance_at(current: &str, guidance_start: usize, guidance_len: usize) -> String {
    let guidance_end = guidance_start + guidance_len;
    let (owned_start, owned_end) = if current[..guidance_start].ends_with("\n\n") {
        (guidance_start - 2, guidance_end)
    } else if current[guidance_end..].starts_with("\n\n") {
        (guidance_start, guidance_end + 2)
    } else if current[..guidance_start].ends_with('\n') {
        (guidance_start - 1, guidance_end)
    } else if current[guidance_end..].starts_with('\n') {
        (guidance_start, guidance_end + 1)
    } else {
        (guidance_start, guidance_end)
    };
    format!("{}{}", &current[..owned_start], &current[owned_end..])
}

fn direct_provider_table(
    profile: &ProviderProfile,
    existing_local_provider: Option<Table>,
) -> Result<Table> {
    let base_url = profile.normalized_base_url();
    if base_url.is_empty() {
        anyhow::bail!("第三方线路缺少 API 地址");
    }
    let preserves_manual_settings = existing_local_provider.is_some();
    let mut provider = existing_local_provider.unwrap_or_default();
    provider["name"] = value(if profile.supports_remote_compaction {
        OPENAI_PROVIDER_NAME
    } else {
        profile.name.trim()
    });
    provider["base_url"] = value(base_url);
    provider["wire_api"] = value("responses");
    if !preserves_manual_settings {
        provider["requires_openai_auth"] = value(true);
    }
    if !profile.api_key.trim().is_empty() {
        provider["experimental_bearer_token"] = value(profile.api_key.trim());
    }
    Ok(provider)
}

fn ensure_supported_provider_protocol(protocol: RelayProtocol) -> Result<()> {
    match protocol {
        RelayProtocol::Responses => Ok(()),
        RelayProtocol::ChatCompletions => anyhow::bail!(
            "当前 Codex 已移除 wire_api = \"chat\"；请将第三方线路改为 Responses API 后重试"
        ),
    }
}

fn ensure_active_provider_uses_responses(doc: &DocumentMut) -> Result<()> {
    let provider_id = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider_id| !provider_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("CC Switch Live 配置缺少活动 model_provider"))?;
    let wire_api = doc
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table_like)
        .and_then(|provider| provider.get("wire_api"))
        .and_then(Item::as_str)
        .unwrap_or("responses")
        .trim();
    if wire_api.eq_ignore_ascii_case("responses") {
        Ok(())
    } else {
        anyhow::bail!(
            "当前 Codex 已移除 wire_api = {wire_api:?}；请将 CC Switch Live 线路改为 Responses API 后重试"
        )
    }
}

fn official_provider_table() -> Table {
    let mut provider = Table::new();
    provider["name"] = value(OPENAI_PROVIDER_NAME);
    provider["base_url"] = value(CHATGPT_CODEX_BASE_URL);
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(true);
    provider
}

fn migrate_legacy_official_provider_name(provider: &mut Item) {
    let is_legacy_official_provider = provider.as_table_like().is_some_and(|provider| {
        provider.get("name").and_then(Item::as_str) == Some(LEGACY_CODEY_GLOBAL_PROVIDER_NAME)
            && provider
                .get("base_url")
                .and_then(Item::as_str)
                .is_some_and(|base_url| {
                    base_url.trim().trim_end_matches('/') == CHATGPT_CODEX_BASE_URL
                })
    });
    if is_legacy_official_provider && let Some(provider) = provider.as_table_like_mut() {
        provider.insert("name", value(OPENAI_PROVIDER_NAME));
    }
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

fn write_global_provider_migration_if_changed(
    home: &Path,
    config_path: &Path,
    existing: &str,
    doc: &DocumentMut,
    original: Option<Vec<u8>>,
) -> Result<()> {
    let updated = document_string(doc)?;
    if updated != existing {
        backup_global_provider_migration(home, original.as_deref())?;
        atomic_write(config_path, updated.as_bytes())?;
    }
    Ok(())
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

fn root_key_string(contents: &str, key: &str) -> Option<String> {
    let doc = contents.parse::<DocumentMut>().ok()?;
    doc.get(key).and_then(Item::as_str).map(ToString::to_string)
}

fn provider_base_url(contents: &str, provider_id: &str) -> Option<String> {
    let doc = contents.parse::<DocumentMut>().ok()?;
    doc.get("model_providers")
        .and_then(Item::as_table)?
        .get(provider_id)
        .and_then(Item::as_table)?
        .get("base_url")
        .and_then(Item::as_str)
        .map(|value| value.trim_end_matches('/').to_string())
}

fn normalized_provider_id(provider_id: &str) -> String {
    let provider_id = provider_id.trim();
    if provider_id.is_empty()
        || provider_id == CODEY_PROVIDER_ID
        || is_reserved_provider(provider_id)
    {
        GLOBAL_PROVIDER_ID.to_string()
    } else {
        provider_id.to_string()
    }
}

fn is_reserved_provider(provider_id: &str) -> bool {
    RESERVED_PROVIDER_IDS.contains(&provider_id)
}

fn backup_global_provider_migration(home: &Path, original: Option<&[u8]>) -> Result<()> {
    let Some(original) = original else {
        return Ok(());
    };
    let backup_root = home.join("backups_state/codey-global-provider");
    create_private_dir_all(&backup_root)?;
    let backup_dir = backup_root.join(format!("{}-{}", timestamp_millis(), std::process::id()));
    create_private_dir_all(&backup_dir)?;
    write_private_file(&backup_dir.join("config.toml"), original)?;
    Ok(())
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
    }
    #[cfg(not(unix))]
    fs::write(path, bytes)?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("路径没有父目录：{}", path.display()))?;
    fs::create_dir_all(parent)?;
    let temp = crate::fs_util::unique_temp_path(path);
    write_private_file(&temp, bytes)?;
    crate::fs_util::persist_temp_file(&temp, path)?;
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("读取文件失败：{}", path.display())),
    }
}

fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("删除文件失败：{}", path.display())),
    }
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
mod tests {
    use super::*;

    #[test]
    fn stale_backup_dirs_are_pruned_beyond_retention() {
        let temp = tempfile::tempdir().unwrap();
        let backup_root = temp.path().join("codex-backups");
        for index in 0..8_u32 {
            fs::create_dir_all(backup_root.join(format!("{}-42", 1000 + index))).unwrap();
        }
        fs::create_dir_all(backup_root.join("unrelated")).unwrap();
        let marker = temp.path().join("codex-lease.json");
        let lease = serde_json::json!({
            "backupDir": backup_root.join("1000-42"),
            "originalConfigExists": true,
        });
        fs::write(&marker, lease.to_string()).unwrap();

        prune_stale_backup_dirs(&backup_root, &marker);

        assert!(backup_root.join("1000-42").is_dir(), "lease dir kept");
        assert!(!backup_root.join("1001-42").is_dir(), "oldest pruned");
        assert!(!backup_root.join("1002-42").is_dir(), "oldest pruned");
        for index in 3..8_u32 {
            assert!(backup_root.join(format!("{}-42", 1000 + index)).is_dir());
        }
        assert!(backup_root.join("unrelated").is_dir(), "foreign dir kept");
    }

    fn official_profile() -> ProviderProfile {
        let mut profile = ProviderProfile::new("OpenAI Official");
        profile.id = "codex-official".to_string();
        profile.cc_switch_read_only = true;
        profile
    }

    fn direct_profile(protocol: RelayProtocol) -> ProviderProfile {
        let mut profile = ProviderProfile::new("Relay");
        profile.base_url = "https://relay.example/v1".to_string();
        profile.api_key = "sk-direct".to_string();
        profile.protocol = protocol;
        profile
    }

    fn relative_model_catalog_path() -> Option<&'static Path> {
        Some(Path::new(crate::model_catalog::relative_path()))
    }

    fn write_legacy_runtime_lease(
        marker: &Path,
        backup_dir: &Path,
        original: Option<&str>,
        provider_id: &str,
        applied_base_url: &str,
    ) {
        fs::create_dir_all(backup_dir).unwrap();
        if let Some(original) = original {
            fs::write(backup_dir.join("config.toml"), original).unwrap();
        }
        write_lease(
            marker,
            &RuntimeConfigLease {
                backup_dir: backup_dir.to_path_buf(),
                config_snapshot_dir: None,
                original_config_exists: original.is_some(),
                preserve_provider_route: false,
                fastctx_command: None,
                subagent_optimization_applied: false,
                subagent_model: String::new(),
                subagent_reasoning_effort: String::new(),
                original_agents_md_exists: false,
                original_default_agent_exists: false,
                original_agents_dir_exists: false,
                provider_id: Some(provider_id.to_string()),
                applied_base_url: Some(applied_base_url.to_string()),
            },
        )
        .unwrap();
    }

    #[test]
    fn official_patch_uses_the_official_endpoint_and_catalog() {
        let result = patch_config(
            "model = \"gpt\"\nmodel_catalog_json = \"old.json\"\n",
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            true,
        )
        .unwrap();
        assert!(result.contains("base_url = \"https://chatgpt.com/backend-api/codex\""));
        assert!(!result.contains("experimental_bearer_token"));
        assert_eq!(
            root_key_string(&result, "model_catalog_json").as_deref(),
            Some("model-catalogs/codey-official.json")
        );
        assert_eq!(root_key_string(&result, "model"), None);
        assert_eq!(
            root_key_string(&result, "service_tier").as_deref(),
            Some("default")
        );
        let document = result.parse::<DocumentMut>().unwrap();
        assert!(
            document["desktop"]["enabled-reasoning-efforts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|effort| effort.as_str() == Some("ultra"))
        );
    }

    #[test]
    fn provider_patch_enables_all_desktop_reasoning_efforts() {
        let existing = r#"
[desktop]
enabled-reasoning-efforts = ["low", "medium", "high", "xhigh"]
"#;
        let result = patch_config(existing, &official_profile(), GLOBAL_PROVIDER_ID, true).unwrap();
        let document = result.parse::<DocumentMut>().unwrap();
        let efforts = document["desktop"]["enabled-reasoning-efforts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|effort| effort.as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max", "ultra"]);
    }

    #[test]
    fn provider_patch_preserves_selected_service_tier() {
        let result = patch_config(
            "service_tier = \"priority\"\n",
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            true,
        )
        .unwrap();

        assert_eq!(
            root_key_string(&result, "service_tier").as_deref(),
            Some("priority")
        );
    }

    #[test]
    fn provider_patch_sets_the_requested_default_model() {
        let result = patch_config_with_fastctx(
            "model = \"old-model\"\n\n[profiles.work]\nmodel = \"profile-model\"\n",
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            Some("gpt-5.6-sol"),
            None,
            false,
        )
        .unwrap();

        assert_eq!(
            root_key_string(&result, "model").as_deref(),
            Some("gpt-5.6-sol")
        );
        let document = result.parse::<DocumentMut>().unwrap();
        let work_profile = document["profiles"]["work"].as_table().unwrap();
        assert!(work_profile.get("model").is_none());
    }

    #[test]
    fn direct_patch_configures_a_responses_provider_without_a_loopback_endpoint() {
        let result = patch_config(
            "model_provider = \"openai\"\n",
            &direct_profile(RelayProtocol::Responses),
            "openai",
            false,
        )
        .unwrap();
        assert!(result.contains("base_url = \"https://relay.example/v1\""));
        assert!(result.contains("wire_api = \"responses\""));
        assert!(result.contains("experimental_bearer_token = \"sk-direct\""));
        assert!(!result.contains("127.0.0.1"));
        assert_eq!(
            root_key_string(&result, "model_provider").as_deref(),
            Some(GLOBAL_PROVIDER_ID)
        );
    }

    #[test]
    fn direct_patch_rejects_the_removed_chat_wire_api() {
        let error = patch_config(
            "model_provider = \"openai\"\n",
            &direct_profile(RelayProtocol::ChatCompletions),
            "openai",
            false,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("当前 Codex 已移除 wire_api = \"chat\"")
        );
    }

    #[test]
    fn route_preserving_patch_keeps_cc_switch_routing_and_model_fields() {
        let existing = r#"
model_provider = "cc-switch-official"
model = "route-model"
model_catalog_json = "/cc-switch/catalog.json"

[model_providers.cc-switch-official]
name = "CC Switch Proxy"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
experimental_bearer_token = "PROXY_MANAGED"

[features.cc_switch_owned]
enabled = true
"#;
        let result = patch_config_with_fastctx_mode(
            existing,
            &direct_profile(RelayProtocol::ChatCompletions),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            Some("codey-model"),
            Some(Path::new("/opt/codey")),
            true,
            DEFAULT_SUBAGENT_MODEL,
            DEFAULT_SUBAGENT_REASONING_EFFORT,
            true,
        )
        .unwrap();
        let before = parse_document(existing).unwrap();
        let after = parse_document(&result).unwrap();

        assert_eq!(
            root_key_string(&result, "model_provider").as_deref(),
            Some("cc-switch-official")
        );
        assert_eq!(
            root_key_string(&result, "model").as_deref(),
            Some("route-model")
        );
        assert_eq!(
            root_key_string(&result, "model_catalog_json").as_deref(),
            Some("/cc-switch/catalog.json")
        );
        assert!(items_semantically_equal(
            before.get("model_providers").unwrap(),
            after.get("model_providers").unwrap()
        ));
        assert!(
            after["features"]["cc_switch_owned"]["enabled"]
                .as_bool()
                .unwrap()
        );
        assert!(
            after["features"]["multi_agent_v2"]["enabled"]
                .as_bool()
                .unwrap()
        );
        assert!(after["mcp_servers"][CODEY_FASTCTX_SERVER_ID].is_table());
    }

    #[test]
    fn route_preserving_patch_rejects_a_live_chat_wire_api() {
        let existing = r#"
model_provider = "cc-switch-live"

[model_providers.cc-switch-live]
name = "CC Switch Live"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "chat"
"#;
        let error = patch_config_with_fastctx_mode(
            existing,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            None,
            None,
            None,
            false,
            DEFAULT_SUBAGENT_MODEL,
            DEFAULT_SUBAGENT_REASONING_EFFORT,
            true,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("请将 CC Switch Live 线路改为 Responses API")
        );
    }

    #[test]
    fn fast_context_tools_reuse_user_fastctx_without_registering_the_embedded_server() {
        let existing = r#"
developer_instructions = "Keep my guidance."
tool_output_token_limit = 16000

[mcp_servers.fastctx]
command = "/custom/fastctx"
args = ["serve", "--enable-shell"]

[features.code_mode]
direct_only_tool_namespaces = ["mcp__existing", "mcp__fastctx"]
"#;
        let result = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/Applications/Codey.app/Contents/MacOS/codey")),
            false,
        )
        .unwrap();
        let document = result.parse::<DocumentMut>().unwrap();

        assert_eq!(
            document["mcp_servers"]["fastctx"]["command"].as_str(),
            Some("/custom/fastctx")
        );
        assert!(
            document["mcp_servers"]
                .as_table()
                .unwrap()
                .get(CODEY_FASTCTX_SERVER_ID)
                .is_none()
        );
        assert_eq!(
            document["tool_output_token_limit"].as_integer(),
            Some(16_000)
        );
        let namespaces = document["features"]["code_mode"]["direct_only_tool_namespaces"]
            .as_array()
            .unwrap();
        assert!(
            namespaces
                .iter()
                .any(|entry| entry.as_str() == Some("mcp__fastctx"))
        );
        assert!(
            namespaces
                .iter()
                .all(|entry| entry.as_str() != Some(CODEY_FASTCTX_NAMESPACE))
        );
        let guidance = document["developer_instructions"].as_str().unwrap();
        assert_eq!(guidance, "Keep my guidance.");
        assert!(!guidance.contains(CODEY_FASTCTX_GUIDANCE));
    }

    #[test]
    fn fast_context_tools_migrate_the_owned_sidecar_to_the_main_executable() {
        let existing = r#"
[mcp_servers.codey_fastctx]
command = "/Applications/Codey.app/Contents/MacOS/codey-fastctx"
args = ["--codey-fastctx-mcp"]
startup_timeout_sec = 15
runtime_note = "preserve"

[mcp_servers.codey_fastctx.env]
CONCURRENT = "preserve"
"#;
        let result = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/Applications/Codey.app/Contents/MacOS/codey")),
            false,
        )
        .unwrap();
        let document = result.parse::<DocumentMut>().unwrap();
        let server = document["mcp_servers"][CODEY_FASTCTX_SERVER_ID]
            .as_table()
            .unwrap();

        assert_eq!(
            server["command"].as_str(),
            Some("/Applications/Codey.app/Contents/MacOS/codey")
        );
        assert_eq!(
            server["args"]
                .as_array()
                .and_then(|arguments| arguments.get(0))
                .and_then(Value::as_str),
            Some("--codey-fastctx-mcp")
        );
        assert_eq!(
            server["startup_timeout_sec"].as_integer(),
            Some(CODEY_FASTCTX_STARTUP_TIMEOUT_SECONDS)
        );
        assert_eq!(server["runtime_note"].as_str(), Some("preserve"));
        assert_eq!(server["env"]["CONCURRENT"].as_str(), Some("preserve"));
        assert_eq!(
            server["env"]["FASTCTX_TOKEN_BUDGET"].as_str(),
            Some(CODEY_FASTCTX_TOKEN_BUDGET)
        );
    }

    #[test]
    fn fast_context_tools_detect_fastctx_invoked_by_another_server_id() {
        let existing = r#"
[mcp_servers.context_tools]
command = "uvx"
args = ["fastctx", "--stdio"]
"#;
        let result = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/tmp/codey")),
            false,
        )
        .unwrap();
        let document = result.parse::<DocumentMut>().unwrap();

        assert!(
            document["mcp_servers"]
                .as_table()
                .unwrap()
                .get(CODEY_FASTCTX_SERVER_ID)
                .is_none()
        );
        assert!(document.get("developer_instructions").is_none());
        assert!(document.get("tool_output_token_limit").is_none());
    }

    #[test]
    fn fast_context_tools_detect_fastctx_in_the_command_case_insensitively() {
        let existing = r#"
[mcp_servers]
context_tools = { command = "/opt/tools/FASTCTX.exe", args = ["--stdio"] }
"#;
        let result = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/tmp/codey")),
            false,
        )
        .unwrap();
        let document = result.parse::<DocumentMut>().unwrap();

        assert!(
            document["mcp_servers"]
                .as_table()
                .unwrap()
                .get(CODEY_FASTCTX_SERVER_ID)
                .is_none()
        );
    }

    #[test]
    fn fast_context_tools_do_not_confuse_fastctx_substrings_with_the_server() {
        let existing = r#"
[mcp_servers.breakfastctx]
command = "/custom/breakfastctx"
"#;
        let result = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/tmp/codey")),
            false,
        )
        .unwrap();
        let document = result.parse::<DocumentMut>().unwrap();

        assert_eq!(
            document["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["command"].as_str(),
            Some("/tmp/codey")
        );
    }

    #[test]
    fn disabling_fast_context_tools_removes_only_codey_owned_artifacts() {
        let original = r#"
developer_instructions = "User guidance."
tool_output_token_limit = 16000

[mcp_servers.user_tools]
command = "/custom/context-server"

[features.code_mode]
direct_only_tool_namespaces = ["mcp__existing"]
"#;
        let enabled = patch_config_with_fastctx(
            original,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/tmp/codey")),
            false,
        )
        .unwrap();
        let mut stale = enabled.parse::<DocumentMut>().unwrap();
        let guidance = stale["developer_instructions"].as_str().unwrap();
        stale["developer_instructions"] = value(format!(
            "{guidance}\n\n{LEGACY_CODEY_FASTCTX_GUIDANCE}\n\nConcurrent guidance."
        ));

        let disabled = patch_config_with_fastctx(
            &document_string(&stale).unwrap(),
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            None,
            false,
        )
        .unwrap();
        let document = disabled.parse::<DocumentMut>().unwrap();

        let mcp_servers = document["mcp_servers"].as_table().unwrap();
        assert!(mcp_servers.get(CODEY_FASTCTX_SERVER_ID).is_none());
        assert_eq!(
            mcp_servers["user_tools"]["command"].as_str(),
            Some("/custom/context-server")
        );
        assert_eq!(
            document["developer_instructions"].as_str(),
            Some("User guidance.\n\nConcurrent guidance.")
        );
        assert_eq!(
            document["tool_output_token_limit"].as_integer(),
            Some(16_000)
        );
        let namespaces = document["features"]["code_mode"]["direct_only_tool_namespaces"]
            .as_array()
            .unwrap();
        assert_eq!(
            namespaces
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            vec!["mcp__existing"]
        );
    }

    #[test]
    fn disabling_fast_context_tools_preserves_a_user_replacement_under_the_reserved_id() {
        let existing = format!(
            r#"developer_instructions = "{CODEY_FASTCTX_GUIDANCE}"

[mcp_servers.codey_fastctx]
command = "/user/server"
args = ["serve"]

[features.code_mode]
direct_only_tool_namespaces = ["mcp__codey_fastctx"]
"#
        );
        let disabled = patch_config_with_fastctx(
            &existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            None,
            false,
        )
        .unwrap();
        let document = disabled.parse::<DocumentMut>().unwrap();

        assert_eq!(
            document["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["command"].as_str(),
            Some("/user/server")
        );
        assert!(document.get("developer_instructions").is_none());
        assert!(
            document["features"]["code_mode"]["direct_only_tool_namespaces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
        );
    }

    #[test]
    fn disabling_fast_context_tools_preserves_an_unproven_user_namespace() {
        let existing = r#"
[features.code_mode]
direct_only_tool_namespaces = ["mcp__codey_fastctx", "mcp__user"]
"#;
        let disabled = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            None,
            false,
        )
        .unwrap();
        let document = disabled.parse::<DocumentMut>().unwrap();
        let namespaces = document["features"]["code_mode"]["direct_only_tool_namespaces"]
            .as_array()
            .unwrap();

        assert_eq!(
            namespaces
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            vec!["mcp__codey_fastctx", "mcp__user"]
        );
    }

    #[test]
    fn fastctx_guidance_cleanup_requires_complete_paragraph_boundaries() {
        let embedded = format!("User prefix {CODEY_FASTCTX_GUIDANCE} user suffix");

        assert_eq!(remove_codey_fastctx_guidance(&embedded), None);
    }

    #[test]
    fn fast_context_tools_are_idempotent_and_default_the_host_output_limit() {
        let first = patch_config_with_fastctx(
            "",
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/tmp/codey")),
            false,
        )
        .unwrap();
        let second = patch_config_with_fastctx(
            &first,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            Some(Path::new("/tmp/codey")),
            false,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.matches(CODEY_FASTCTX_GUIDANCE).count(), 1);
        let document = first.parse::<DocumentMut>().unwrap();
        assert_eq!(
            document["features"]["code_mode"]["direct_only_tool_namespaces"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
                .count(),
            1
        );
        assert_eq!(
            document["tool_output_token_limit"].as_integer(),
            Some(10_000)
        );
    }

    #[test]
    fn subagent_optimization_enables_v2_and_removes_legacy_agents() {
        let existing = r#"
[agents]
max_threads = 6
max_depth = 1
interrupt_message = true

[features.multi_agent_v2]
enabled = false
custom_setting = "preserved"
"#;
        let result = patch_config_with_fastctx_mode(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            relative_model_catalog_path(),
            None,
            None,
            true,
            "gpt-5.6-sol",
            "high",
            false,
        )
        .unwrap();
        let document = result.parse::<DocumentMut>().unwrap();
        let agents = document["agents"].as_table().unwrap();
        let multi_agent = document["features"]["multi_agent_v2"].as_table().unwrap();

        assert!(agents.get("max_threads").is_none());
        assert!(agents.get("max_depth").is_none());
        assert!(agents.get("interrupt_message").is_none());
        assert_eq!(
            agents["default_subagent_model"].as_str(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            agents["default_subagent_reasoning_effort"].as_str(),
            Some("high")
        );
        assert_eq!(multi_agent["enabled"].as_bool(), Some(true));
        assert_eq!(
            multi_agent["hide_spawn_agent_metadata"].as_bool(),
            Some(true)
        );
        assert_eq!(multi_agent["tool_namespace"].as_str(), Some("agents"));
        assert_eq!(
            multi_agent["max_concurrent_threads_per_session"].as_integer(),
            Some(7)
        );
        assert_eq!(
            multi_agent["min_wait_timeout_ms"].as_integer(),
            Some(10_000)
        );
        assert_eq!(
            multi_agent["default_wait_timeout_ms"].as_integer(),
            Some(30_000)
        );
        assert_eq!(
            multi_agent["max_wait_timeout_ms"].as_integer(),
            Some(120_000)
        );
        assert_eq!(multi_agent["custom_setting"].as_str(), Some("preserved"));
    }

    #[test]
    fn subagent_lease_applies_and_restores_all_owned_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(home.join("agents")).unwrap();
        let original_config = b"model_provider = \"codey_global\"\n\n[agents]\nmax_threads = 3\n\n[model_providers.codey_global]\nbase_url = \"https://chatgpt.com/backend-api/codex\"\n";
        let original_agents_md = b"# Existing guidance\n\nKeep this verbatim.\n";
        let original_default_agent = b"name = \"custom\"\nmodel = \"custom-model\"\n";
        fs::write(home.join("config.toml"), original_config).unwrap();
        fs::write(home.join("AGENTS.md"), original_agents_md).unwrap();
        fs::write(home.join("agents/default.toml"), original_default_agent).unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            true,
            &marker,
            &backup_root,
        )
        .unwrap();

        let temporary_config = fs::read_to_string(home.join("config.toml")).unwrap();
        let document = temporary_config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            document["agents"]["default_subagent_model"].as_str(),
            Some(DEFAULT_SUBAGENT_MODEL)
        );
        assert_eq!(
            document["agents"]["default_subagent_reasoning_effort"].as_str(),
            Some(DEFAULT_SUBAGENT_REASONING_EFFORT)
        );
        assert_eq!(
            document["model_catalog_json"].as_str(),
            Some(
                home.join(crate::model_catalog::relative_path())
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(
            document["features"]["multi_agent_v2"]["tool_namespace"].as_str(),
            Some("agents")
        );
        assert!(
            fs::read_to_string(home.join("AGENTS.md"))
                .unwrap()
                .contains(SUBAGENT_GUIDANCE)
        );
        assert_eq!(
            fs::read_to_string(home.join("agents/default.toml")).unwrap(),
            DEFAULT_AGENT_CONFIG
        );

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), original_config);
        assert_eq!(
            fs::read(home.join("AGENTS.md")).unwrap(),
            original_agents_md
        );
        assert_eq!(
            fs::read(home.join("agents/default.toml")).unwrap(),
            original_default_agent
        );
        assert!(!marker.exists());
    }

    #[test]
    fn hot_reloaded_subagent_defaults_are_adopted_by_runtime_lease() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        let original_config = b"model_provider = \"codey_global\"\n\n[agents]\ncustom = \"keep\"\n\n[model_providers.codey_global]\nbase_url = \"https://chatgpt.com/backend-api/codex\"\n";
        fs::write(home.join("config.toml"), original_config).unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            true,
            &marker,
            &backup_root,
        )
        .unwrap();

        let mut current = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        current["agents"]["default_subagent_model"] = value("gpt-5.6-sol");
        current["agents"]["default_subagent_reasoning_effort"] = value("high");
        fs::write(home.join("config.toml"), document_string(&current).unwrap()).unwrap();

        mark_runtime_subagent_defaults_applied_at(&home, &marker, "gpt-5.6-sol", "high").unwrap();

        let state =
            serde_json::from_str::<RuntimeConfigLease>(&fs::read_to_string(&marker).unwrap())
                .unwrap();
        assert_eq!(state.subagent_model, "gpt-5.6-sol");
        assert_eq!(state.subagent_reasoning_effort, "high");
        let snapshot_dir = state
            .config_snapshot_dir
            .as_deref()
            .unwrap_or(&state.backup_dir);
        let applied = fs::read_to_string(snapshot_dir.join(APPLIED_CONFIG_FILE))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            applied["agents"]["default_subagent_model"].as_str(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            applied["agents"]["default_subagent_reasoning_effort"].as_str(),
            Some("high")
        );

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), original_config);
    }

    #[test]
    fn subagent_lease_preserves_concurrent_user_file_changes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"codey_global\"\n",
        )
        .unwrap();
        fs::write(home.join("AGENTS.md"), "# Original\n").unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            true,
            &marker,
            &backup_root,
        )
        .unwrap();
        let mut concurrent_agents_md = fs::read_to_string(home.join("AGENTS.md")).unwrap();
        concurrent_agents_md.push_str("\n## User addition\nKeep this too.\n");
        fs::write(home.join("AGENTS.md"), concurrent_agents_md).unwrap();
        fs::write(
            home.join("agents/default.toml"),
            "name = \"user-replacement\"\n",
        )
        .unwrap();

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        let restored_agents_md = fs::read_to_string(home.join("AGENTS.md")).unwrap();
        assert!(restored_agents_md.contains("# Original"));
        assert!(restored_agents_md.contains("## User addition"));
        assert!(!restored_agents_md.contains(SUBAGENT_GUIDANCE));
        assert_eq!(
            fs::read_to_string(home.join("agents/default.toml")).unwrap(),
            "name = \"user-replacement\"\n"
        );
    }

    #[test]
    fn subagent_lease_removes_runtime_only_files_on_restore() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"codey_global\"\n",
        )
        .unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            true,
            &marker,
            &backup_root,
        )
        .unwrap();
        assert!(home.join("AGENTS.md").exists());
        assert!(home.join("agents/default.toml").exists());

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert!(!home.join("AGENTS.md").exists());
        assert!(!home.join("agents/default.toml").exists());
        assert!(!home.join("agents").exists());
    }

    #[test]
    fn subagent_lease_restores_owned_files_after_a_provider_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"codey_global\"\n",
        )
        .unwrap();
        let original_agents_md = b"# Original guidance\n";
        fs::write(home.join("AGENTS.md"), original_agents_md).unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            true,
            &marker,
            &backup_root,
        )
        .unwrap();
        let replacement_config = b"model_provider = \"user-provider\"\n\n[model_providers.user-provider]\nbase_url = \"https://user.example/v1\"\n";
        fs::write(home.join("config.toml"), replacement_config).unwrap();

        assert!(!restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert_eq!(
            fs::read(home.join("config.toml")).unwrap(),
            replacement_config
        );
        assert_eq!(
            fs::read(home.join("AGENTS.md")).unwrap(),
            original_agents_md
        );
        assert!(!home.join("agents/default.toml").exists());
        assert!(!marker.exists());
    }

    #[test]
    fn lease_restores_the_exact_original_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        let original = b"model_provider = \"codey_global\"\n\n[model_providers.codey_global]\nbase_url = \"https://chatgpt.com/backend-api/codex\"\n";
        fs::write(home.join("config.toml"), original).unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            false,
            &marker,
            &backup_root,
        )
        .unwrap();
        let temporary = fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(
            provider_base_url(&temporary, GLOBAL_PROVIDER_ID).as_deref(),
            Some("https://relay.example/v1")
        );
        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), original);
        assert!(!marker.exists());
    }

    #[test]
    fn route_lease_rebases_after_cc_switch_hot_swap_and_restores_latest_route() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        let route_a = r#"
model_provider = "route-a"
model = "model-a"
model_catalog_json = "/cc-switch/catalog-a.json"

[model_providers.route-a]
name = "Route A"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
experimental_bearer_token = "PROXY_MANAGED"

[mcp_servers.cc-switch]
command = "cc-switch-tool"
"#;
        fs::write(home.join("config.toml"), route_a).unwrap();

        apply_runtime_provider_config_at_mode(
            &home,
            &direct_profile(RelayProtocol::Responses),
            "route-a",
            false,
            None,
            Some(Path::new("/opt/codey")),
            false,
            DEFAULT_SUBAGENT_MODEL,
            DEFAULT_SUBAGENT_REASONING_EFFORT,
            &marker,
            &backup_root,
            true,
        )
        .unwrap();
        let applied_a = fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(
            root_key_string(&applied_a, "model_provider").as_deref(),
            Some("route-a")
        );
        assert_eq!(
            root_key_string(&applied_a, "model").as_deref(),
            Some("model-a")
        );

        let route_b = r#"
model_provider = "route-b"
model = "model-b"
model_catalog_json = "/cc-switch/catalog-b.json"
cc_switch_generation = 2

[model_providers.route-b]
name = "Route B"
base_url = "http://localhost:15721/v1"
wire_api = "responses"
experimental_bearer_token = "PROXY_MANAGED"

[mcp_servers.cc-switch]
command = "cc-switch-tool-v2"
"#;
        fs::write(home.join("config.toml"), route_b).unwrap();

        let reconciled = reconcile_runtime_config_overlay_at(&home, &marker)
            .unwrap()
            .unwrap();
        let applied_b = String::from_utf8(reconciled).unwrap();
        assert_eq!(
            root_key_string(&applied_b, "model_provider").as_deref(),
            Some("route-b")
        );
        assert_eq!(
            root_key_string(&applied_b, "model").as_deref(),
            Some("model-b")
        );
        assert_eq!(
            root_key_string(&applied_b, "model_catalog_json").as_deref(),
            Some("/cc-switch/catalog-b.json")
        );
        assert_eq!(
            provider_base_url(&applied_b, "route-b").as_deref(),
            Some("http://localhost:15721/v1")
        );
        let applied_b_doc = parse_document(&applied_b).unwrap();
        assert_eq!(
            applied_b_doc["mcp_servers"]["cc-switch"]["command"].as_str(),
            Some("cc-switch-tool-v2")
        );
        assert!(
            applied_b_doc["mcp_servers"][CODEY_FASTCTX_SERVER_ID]
                .as_table()
                .is_some()
        );

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        let restored = fs::read_to_string(home.join("config.toml")).unwrap();
        let expected = parse_document(route_b).unwrap();
        let actual = parse_document(&restored).unwrap();
        assert!(tables_semantically_equal(
            expected.as_table(),
            actual.as_table()
        ));
        assert!(!marker.exists());
    }

    #[test]
    fn first_direct_runtime_lease_preserves_chatgpt_auth_json() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        let auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"free-account-token"}}"#;
        fs::write(home.join("auth.json"), auth).unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            false,
            None,
            None,
            false,
            &marker,
            &backup_root,
        )
        .unwrap();

        let temporary = fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(
            provider_base_url(&temporary, GLOBAL_PROVIDER_ID).as_deref(),
            Some("https://relay.example/v1")
        );
        assert!(temporary.contains("experimental_bearer_token = \"sk-direct\""));
        assert!(!temporary.contains("base_url = \"https://chatgpt.com/backend-api/codex\""));
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), auth);

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert!(!home.join("config.toml").exists());
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), auth);
    }

    #[test]
    fn manual_local_provider_settings_survive_the_runtime_lease() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        let original = br#"model_provider = "manual"

[model_providers.manual]
name = "Manual Relay"
base_url = "https://manual.example/v1"
wire_api = "responses"
requires_openai_auth = false
env_key = "MANUAL_RELAY_API_KEY"
request_max_retries = 7

[model_providers.manual.http_headers]
X-Route = "manual"
"#;
        let auth = br#"{"auth_mode":"chatgpt","tokens":{"access_token":"free-account-token"}}"#;
        fs::write(home.join("config.toml"), original).unwrap();
        fs::write(home.join("auth.json"), auth).unwrap();
        let mut profile = ProviderProfile::new("Manual Relay");
        profile.id = "manual".to_string();
        profile.base_url = "https://manual.example/v1".to_string();

        apply_runtime_provider_config_at(
            &home,
            &profile,
            "manual",
            false,
            None,
            None,
            false,
            &marker,
            &backup_root,
        )
        .unwrap();

        let temporary = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let provider = temporary["model_providers"]["manual"].as_table().unwrap();
        assert_eq!(
            provider["base_url"].as_str(),
            Some("https://manual.example/v1")
        );
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(false));
        assert_eq!(provider["env_key"].as_str(), Some("MANUAL_RELAY_API_KEY"));
        assert_eq!(provider["request_max_retries"].as_integer(), Some(7));
        assert_eq!(provider["http_headers"]["X-Route"].as_str(), Some("manual"));
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), auth);

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), original);
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), auth);
    }

    #[test]
    fn legacy_lease_reverts_owned_fields_without_overwriting_concurrent_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_dir = temp.path().join("codey/codex-backups/legacy");
        fs::create_dir_all(&home).unwrap();
        let original = r#"model_provider = "openai"
model = "gpt-original"
model_catalog_json = "user-catalog.json"
profile = "work"
developer_instructions = "Original guidance"

[model_providers.codey_global]
name = "Original provider"
base_url = "https://chatgpt.com/backend-api/codex"
wire_api = "responses"
requires_openai_auth = true
custom_original = "restore"

[desktop]
enabled-reasoning-efforts = ["medium"]

[profiles.work]
model = "profile-original"

[mcp_servers.codey_fastctx]
command = "/user/server"
args = ["serve"]
custom_original = "restore"

[features.code_mode]
direct_only_tool_namespaces = ["mcp__existing"]
"#;
        let current = format!(
            r#"model_provider = "codey_global"
model_catalog_json = "model-catalogs/codey-official.json"
profile = "work"
developer_instructions = "Original guidance\n\n{LEGACY_CODEY_FASTCTX_GUIDANCE}\n\n{CODEY_FASTCTX_GUIDANCE}\n\nConcurrent guidance"
tool_output_token_limit = 10000
approval_policy = "never"
service_tier = "fast"

[model_providers.codey_global]
name = "Relay"
base_url = "https://relay.example/v1"
wire_api = "chat"
requires_openai_auth = true
experimental_bearer_token = "sk-temporary"
runtime_note = "preserve"

[desktop]
enabled-reasoning-efforts = ["low", "medium", "high", "xhigh"]

[profiles.work]
approval_policy = "never"

[mcp_servers.codey_fastctx]
command = "/Applications/Codey.app/Contents/MacOS/codey"
args = ["--codey-fastctx-mcp"]
startup_timeout_sec = 120
tool_timeout_sec = 120
runtime_note = "preserve"

[mcp_servers.codey_fastctx.env]
FASTCTX_TOKEN_BUDGET = "8500"
CONCURRENT = "preserve"

[features.code_mode]
direct_only_tool_namespaces = ["mcp__existing", "mcp__codey_fastctx", "mcp__concurrent"]

[marketplaces.openai-bundled]
last_updated = "new"
"#
        );
        fs::write(home.join("config.toml"), current).unwrap();
        write_legacy_runtime_lease(
            &marker,
            &backup_dir,
            Some(original),
            GLOBAL_PROVIDER_ID,
            "https://relay.example/v1",
        );

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        let restored = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();

        assert_eq!(restored["model_provider"].as_str(), Some("openai"));
        assert_eq!(restored["model"].as_str(), Some("gpt-original"));
        assert_eq!(
            restored["model_catalog_json"].as_str(),
            Some("user-catalog.json")
        );
        assert_eq!(
            restored["developer_instructions"].as_str(),
            Some("Original guidance\n\nConcurrent guidance")
        );
        assert!(restored.get("tool_output_token_limit").is_none());
        assert_eq!(restored["approval_policy"].as_str(), Some("never"));
        assert_eq!(restored["service_tier"].as_str(), Some("fast"));

        let provider = restored["model_providers"][GLOBAL_PROVIDER_ID]
            .as_table()
            .unwrap();
        assert_eq!(provider["name"].as_str(), Some("Original provider"));
        assert_eq!(provider["base_url"].as_str(), Some(CHATGPT_CODEX_BASE_URL));
        assert_eq!(provider["wire_api"].as_str(), Some("responses"));
        assert!(provider.get("experimental_bearer_token").is_none());
        assert_eq!(provider["custom_original"].as_str(), Some("restore"));
        assert_eq!(provider["runtime_note"].as_str(), Some("preserve"));

        let efforts = restored["desktop"]["enabled-reasoning-efforts"]
            .as_array()
            .unwrap();
        assert_eq!(
            efforts.iter().filter_map(Value::as_str).collect::<Vec<_>>(),
            vec!["medium"]
        );
        assert_eq!(
            restored["profiles"]["work"]["model"].as_str(),
            Some("profile-original")
        );
        assert_eq!(
            restored["profiles"]["work"]["approval_policy"].as_str(),
            Some("never")
        );

        let fastctx = restored["mcp_servers"][CODEY_FASTCTX_SERVER_ID]
            .as_table()
            .unwrap();
        assert_eq!(fastctx["command"].as_str(), Some("/user/server"));
        assert_eq!(fastctx["args"][0].as_str(), Some("serve"));
        assert!(fastctx.get("startup_timeout_sec").is_none());
        assert!(fastctx.get("tool_timeout_sec").is_none());
        assert_eq!(fastctx["custom_original"].as_str(), Some("restore"));
        assert_eq!(fastctx["runtime_note"].as_str(), Some("preserve"));
        assert!(fastctx["env"].get("FASTCTX_TOKEN_BUDGET").is_none());
        assert_eq!(fastctx["env"]["CONCURRENT"].as_str(), Some("preserve"));

        let namespaces = restored["features"]["code_mode"]["direct_only_tool_namespaces"]
            .as_array()
            .unwrap();
        assert!(
            namespaces
                .iter()
                .all(|entry| entry.as_str() != Some(CODEY_FASTCTX_NAMESPACE))
        );
        assert!(
            namespaces
                .iter()
                .any(|entry| entry.as_str() == Some("mcp__existing"))
        );
        assert!(
            namespaces
                .iter()
                .any(|entry| entry.as_str() == Some("mcp__concurrent"))
        );
        assert_eq!(
            restored["marketplaces"]["openai-bundled"]["last_updated"].as_str(),
            Some("new")
        );
        assert!(!marker.exists());
    }

    #[test]
    fn legacy_lease_preserves_a_new_user_config_when_no_original_existed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_dir = temp.path().join("codey/codex-backups/legacy");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            r#"model_provider = "codey_global"
approval_policy = "never"

[model_providers.codey_global]
name = "Relay"
base_url = "https://relay.example/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-temporary"

[plugins.browser]
enabled = true
"#,
        )
        .unwrap();
        write_legacy_runtime_lease(
            &marker,
            &backup_dir,
            None,
            GLOBAL_PROVIDER_ID,
            "https://relay.example/v1",
        );

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        let restored = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert!(restored.get("model_provider").is_none());
        assert!(restored.get("model_providers").is_none());
        assert_eq!(restored["approval_policy"].as_str(), Some("never"));
        assert_eq!(
            restored["plugins"]["browser"]["enabled"].as_bool(),
            Some(true)
        );
        assert!(!marker.exists());
    }

    #[test]
    fn legacy_lease_removes_a_runtime_only_config_when_no_original_existed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_dir = temp.path().join("codey/codex-backups/legacy");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            r#"model_provider = "codey_global"

[model_providers.codey_global]
name = "Relay"
base_url = "https://relay.example/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-temporary"
"#,
        )
        .unwrap();
        write_legacy_runtime_lease(
            &marker,
            &backup_dir,
            None,
            GLOBAL_PROVIDER_ID,
            "https://relay.example/v1",
        );

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert!(!home.join("config.toml").exists());
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn lease_snapshots_use_private_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"codey_global\"\n",
        )
        .unwrap();

        let backup_dir = apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            false,
            &marker,
            &backup_root,
        )
        .unwrap();

        for path in [&backup_root, &backup_dir] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700,
                "{} should only be accessible by its owner",
                path.display()
            );
        }
        for path in [
            backup_dir.join("config.toml"),
            backup_dir.join(APPLIED_CONFIG_FILE),
            marker,
            home.join("config.toml"),
        ] {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "{} should only be readable and writable by its owner",
                path.display()
            );
        }
    }

    #[test]
    fn lease_preserves_concurrent_codex_updates_while_reverting_codey_fields() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            r#"model_provider = "codey_global"
model = "gpt-old"

[model_providers.codey_global]
name = "Original"
base_url = "https://chatgpt.com/backend-api/codex"

[marketplaces.openai-bundled]
last_updated = "old"
"#,
        )
        .unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            Some(Path::new("/tmp/codey")),
            false,
            &marker,
            &backup_root,
        )
        .unwrap();

        let mut current = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        current["model"] = value("gpt-new");
        current["service_tier"] = value("fast");
        current["developer_instructions"] = value(format!(
            "{}\n\nKeep concurrent guidance.",
            current["developer_instructions"].as_str().unwrap()
        ));
        current["features"]["code_mode"]["direct_only_tool_namespaces"]
            .as_array_mut()
            .unwrap()
            .push("mcp__concurrent");
        current["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["runtime_note"] = value("concurrent field");
        let marketplaces = ensure_root_table(&mut current, "marketplaces").unwrap();
        let mut bundled = Table::new();
        bundled["last_updated"] = value("new");
        marketplaces["openai-bundled"] = Item::Table(bundled);
        let plugins = ensure_root_table(&mut current, "plugins").unwrap();
        let mut browser = Table::new();
        browser["enabled"] = value(true);
        plugins["browser@openai-bundled"] = Item::Table(browser);
        atomic_write(
            &home.join("config.toml"),
            document_string(&current).unwrap().as_bytes(),
        )
        .unwrap();

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        let restored = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();

        assert_eq!(restored["model"].as_str(), Some("gpt-new"));
        assert_eq!(restored["service_tier"].as_str(), Some("fast"));
        assert_eq!(
            restored["developer_instructions"].as_str(),
            Some("Keep concurrent guidance.")
        );
        let namespaces = restored["features"]["code_mode"]["direct_only_tool_namespaces"]
            .as_array()
            .unwrap();
        assert!(
            namespaces
                .iter()
                .all(|entry| entry.as_str() != Some(CODEY_FASTCTX_NAMESPACE))
        );
        assert!(
            namespaces
                .iter()
                .any(|entry| entry.as_str() == Some("mcp__concurrent"))
        );
        assert_eq!(
            restored["marketplaces"]["openai-bundled"]["last_updated"].as_str(),
            Some("new")
        );
        assert_eq!(
            restored["plugins"]["browser@openai-bundled"]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(
            restored["model_providers"][GLOBAL_PROVIDER_ID]["base_url"].as_str(),
            Some(CHATGPT_CODEX_BASE_URL)
        );
        assert!(restored.get("model_catalog_json").is_none());
        assert!(
            restored
                .get("mcp_servers")
                .and_then(Item::as_table)
                .and_then(|servers| servers.get(CODEY_FASTCTX_SERVER_ID))
                .is_none()
        );
        assert!(!marker.exists());
    }

    #[test]
    fn restore_preserves_a_concurrent_replacement_of_the_reserved_fastctx_server() {
        let applied = r#"
[mcp_servers.codey_fastctx]
command = "/Applications/Codey.app/Contents/MacOS/codey"
args = ["--codey-fastctx-mcp"]
startup_timeout_sec = 15
"#;
        let current = r#"
[mcp_servers.codey_fastctx]
command = "/custom/server"
args = ["serve"]
note = "user replacement"
"#;

        let restored = restore_owned_config_changes("", applied, current)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();

        assert_eq!(
            restored["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["command"].as_str(),
            Some("/custom/server")
        );
        assert_eq!(
            restored["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["args"][0].as_str(),
            Some("serve")
        );
        assert_eq!(
            restored["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["note"].as_str(),
            Some("user replacement")
        );
    }

    #[test]
    fn lease_preserves_plugin_install_metadata_across_relaunches() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"codey_global\"\n",
        )
        .unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            false,
            &marker,
            &backup_root,
        )
        .unwrap();

        let mut current = fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let marketplaces = ensure_root_table(&mut current, "marketplaces").unwrap();
        let mut bundled = Table::new();
        bundled["source_type"] = value("local");
        bundled["source"] = value("/tmp/openai-bundled");
        bundled["last_updated"] = value("2026-07-21T09:00:00Z");
        marketplaces["openai-bundled"] = Item::Table(bundled);
        let plugins = ensure_root_table(&mut current, "plugins").unwrap();
        let mut browser = Table::new();
        browser["enabled"] = value(true);
        browser["version"] = value("26.715.52143");
        browser["install_path"] = value("/tmp/plugins/browser/26.715.52143");
        plugins["browser@openai-bundled"] = Item::Table(browser);
        atomic_write(
            &home.join("config.toml"),
            document_string(&current).unwrap().as_bytes(),
        )
        .unwrap();

        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        let first_restore = fs::read(home.join("config.toml")).unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            false,
            &marker,
            &backup_root,
        )
        .unwrap();
        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), first_restore);

        let restored = String::from_utf8(first_restore)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            restored["marketplaces"]["openai-bundled"]["last_updated"].as_str(),
            Some("2026-07-21T09:00:00Z")
        );
        assert_eq!(
            restored["plugins"]["browser@openai-bundled"]["version"].as_str(),
            Some("26.715.52143")
        );
        assert_eq!(
            restored["plugins"]["browser@openai-bundled"]["install_path"].as_str(),
            Some("/tmp/plugins/browser/26.715.52143")
        );
    }

    #[test]
    fn installs_a_non_reserved_global_provider_for_builtin_openai() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"openai\"\nmodel = \"gpt-5\"\n",
        )
        .unwrap();
        assert_eq!(
            ensure_global_model_provider(&home).unwrap(),
            GLOBAL_PROVIDER_ID
        );
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(
            provider_base_url(&config, GLOBAL_PROVIDER_ID).as_deref(),
            Some(CHATGPT_CODEX_BASE_URL)
        );
        let document = config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            document["model_providers"][GLOBAL_PROVIDER_ID]["name"].as_str(),
            Some("OpenAI")
        );
        assert!(!config.contains("[model_providers.openai]"));
    }

    #[test]
    fn migrates_the_legacy_codey_name_for_the_official_provider() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            r#"model_provider = "codey_global"

[model_providers.codey_global]
name = "OpenAI (Codey Global)"
base_url = "https://chatgpt.com/backend-api/codex/"
wire_api = "responses"
requires_openai_auth = true
"#,
        )
        .unwrap();

        assert_eq!(
            ensure_global_model_provider(&home).unwrap(),
            GLOBAL_PROVIDER_ID
        );
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        let document = config.parse::<DocumentMut>().unwrap();
        let provider = document["model_providers"][GLOBAL_PROVIDER_ID]
            .as_table_like()
            .unwrap();

        assert_eq!(
            provider.get("name").and_then(Item::as_str),
            Some(OPENAI_PROVIDER_NAME)
        );
        assert_eq!(
            provider.get("base_url").and_then(Item::as_str),
            Some("https://chatgpt.com/backend-api/codex/")
        );
    }

    #[test]
    fn migrates_a_reserved_custom_provider_without_changing_its_api_address() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            r#"model_provider = "openai"

[model_providers.openai]
name = "Private Relay"
base_url = "https://relay.example/v1"
wire_api = "chat"
requires_openai_auth = true
experimental_bearer_token = "sk-existing"
"#,
        )
        .unwrap();

        assert_eq!(
            ensure_global_model_provider(&home).unwrap(),
            GLOBAL_PROVIDER_ID
        );
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        let document = config.parse::<DocumentMut>().unwrap();
        let provider = document["model_providers"][GLOBAL_PROVIDER_ID]
            .as_table_like()
            .unwrap();

        assert_eq!(
            provider.get("name").and_then(Item::as_str),
            Some("Private Relay")
        );
        assert_eq!(
            provider.get("base_url").and_then(Item::as_str),
            Some("https://relay.example/v1")
        );
        assert_eq!(
            provider.get("wire_api").and_then(Item::as_str),
            Some("chat")
        );
        assert_eq!(
            provider
                .get("experimental_bearer_token")
                .and_then(Item::as_str),
            Some("sk-existing")
        );
        assert!(
            document["model_providers"]
                .as_table()
                .unwrap()
                .get("openai")
                .is_none()
        );
    }

    #[test]
    fn preserves_an_existing_global_provider_api_address() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        let original = r#"model_provider = "codey_global"

[model_providers.codey_global]
name = "Private Relay"
base_url = "https://relay.example/v1"
wire_api = "responses"
requires_openai_auth = true
experimental_bearer_token = "sk-existing"
"#;
        fs::write(home.join("config.toml"), original).unwrap();

        assert_eq!(
            ensure_global_model_provider(&home).unwrap(),
            GLOBAL_PROVIDER_ID
        );
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            original
        );
    }

    #[test]
    fn preserves_an_existing_non_reserved_provider() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        let original = "model_provider = \"company\"\n\n[model_providers.company]\nname = \"Company\"\nbase_url = \"https://example.com/v1\"\n";
        fs::write(home.join("config.toml"), original).unwrap();
        assert_eq!(
            ensure_global_model_provider(&home).unwrap(),
            "company".to_string()
        );
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            original
        );
    }
}
