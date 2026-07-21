use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use codex_plus_core::settings::RelayProtocol;
use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, Item, Table, Value, value};

use crate::config::{ProviderProfile, default_config_path};
use crate::provider_lease::CODEY_PROVIDER_ID;

pub const GLOBAL_PROVIDER_ID: &str = "codey_global";
pub const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
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

description = "General-purpose subagent locked to gpt-5.6-luna with low reasoning."

model = "gpt-5.6-luna"

model_reasoning_effort = "low"

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
    original_config_exists: bool,
    #[serde(default)]
    subagent_optimization_applied: bool,
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
    codex_plus_core::relay_config::default_codex_home_dir()
}

pub fn lease_marker_path() -> PathBuf {
    default_config_path()
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join("codex-lease.json")
}

pub fn apply_runtime_provider_config(
    home: &Path,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
    default_model: Option<&str>,
    fast_context_tools: bool,
    subagent_optimization: bool,
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
    apply_runtime_provider_config_at(
        home,
        profile,
        provider_id,
        use_official_catalog,
        default_model,
        fastctx_command.as_deref(),
        subagent_optimization,
        &marker,
        &backup_root,
    )
}

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
    let backup_dir = backup_root.join(format!("{}-{}", timestamp_millis(), std::process::id()));
    create_private_dir_all(&backup_dir)?;
    if let Some(bytes) = original_config.as_deref() {
        write_private_file(&backup_dir.join("config.toml"), bytes)?;
    }

    let existing = String::from_utf8(original_config.clone().unwrap_or_default())
        .context("Codex config.toml 不是 UTF-8")?;
    let updated_agents_md = if subagent_optimization {
        let existing_agents_md = String::from_utf8(original_agents_md.clone().unwrap_or_default())
            .context("Codex AGENTS.md 不是 UTF-8")?;
        Some(append_subagent_guidance(&existing_agents_md))
    } else {
        None
    };
    let provider_id = normalized_provider_id(provider_id);
    let updated = patch_config_with_fastctx(
        &existing,
        profile,
        &provider_id,
        use_official_catalog,
        default_model,
        fastctx_command,
        subagent_optimization,
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
        original_config_exists: original_config.is_some(),
        subagent_optimization_applied: subagent_optimization,
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
    if !provider_matches || !endpoint_matches {
        restore_runtime_subagent_files(home, &state)?;
        remove_optional(marker)?;
        return Ok(false);
    }

    let backup_config = state.backup_dir.join("config.toml");
    let original = if state.original_config_exists {
        fs::read_to_string(&backup_config)
            .with_context(|| format!("找不到 Codex 原配置备份：{}", backup_config.display()))?
    } else {
        String::new()
    };
    let applied_config = state.backup_dir.join(APPLIED_CONFIG_FILE);
    if applied_config.exists() {
        let applied = fs::read_to_string(&applied_config).with_context(|| {
            format!(
                "读取 Codey 已应用配置快照失败：{}",
                applied_config.display()
            )
        })?;
        let restored = restore_owned_config_changes(&original, &applied, &current)?;
        if !state.original_config_exists && restored.trim().is_empty() {
            remove_optional(&config_path)?;
        } else {
            atomic_write(&config_path, restored.as_bytes())?;
        }
    } else if state.original_config_exists {
        // Legacy leases predate the applied snapshot and cannot distinguish
        // Codex-managed changes from Codey's temporary edits.
        atomic_write(&config_path, original.as_bytes())?;
    } else {
        remove_optional(&config_path)?;
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
                        optional_items_semantically_equal(applied.get(*field), current.get(*field))
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
            let original_has_guidance = original
                .and_then(Item::as_str)
                .is_some_and(|text| text.contains(CODEY_FASTCTX_GUIDANCE));
            let applied_has_guidance = applied
                .and_then(Item::as_str)
                .is_some_and(|text| text.contains(CODEY_FASTCTX_GUIDANCE));
            if original_has_guidance || !applied_has_guidance {
                return false;
            }
            let Some(current) = current else {
                return false;
            };
            let Some(text) = current.as_str() else {
                return false;
            };
            let separator_and_guidance = format!("\n\n{CODEY_FASTCTX_GUIDANCE}");
            let restored = if text.contains(&separator_and_guidance) {
                text.replacen(&separator_and_guidance, "", 1)
            } else if let Some(remainder) = text.strip_prefix(CODEY_FASTCTX_GUIDANCE) {
                remainder.trim_start_matches('\n').to_string()
            } else if text.contains(CODEY_FASTCTX_GUIDANCE) {
                text.replacen(CODEY_FASTCTX_GUIDANCE, "", 1)
            } else {
                return false;
            };
            *current = value(restored);
            true
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

    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        for provider in RESERVED_PROVIDER_IDS {
            providers.remove(provider);
        }
    }
    let current_provider = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(ToString::to_string);
    if let Some(provider) = current_provider.as_deref()
        && !is_reserved_provider(provider)
        && provider != CODEY_PROVIDER_ID
        && provider != GLOBAL_PROVIDER_ID
    {
        write_global_provider_migration_if_changed(home, &config_path, &existing, &doc, original)?;
        return Ok(provider.to_string());
    }

    ensure_provider_table(&mut doc)?;
    doc["model_providers"]
        .as_table_mut()
        .expect("model_providers was initialized")[GLOBAL_PROVIDER_ID] =
        Item::Table(official_provider_table());
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
    patch_config_with_fastctx(
        existing,
        profile,
        provider_id,
        use_official_catalog,
        None,
        None,
        false,
    )
}

fn patch_config_with_fastctx(
    existing: &str,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
    default_model: Option<&str>,
    fastctx_command: Option<&Path>,
    subagent_optimization: bool,
) -> Result<String> {
    let mut doc = parse_document(existing)?;
    ensure_provider_table(&mut doc)?;
    let provider_id = normalized_provider_id(provider_id);
    let provider = if profile.cc_switch_read_only {
        official_provider_table()
    } else {
        direct_provider_table(profile)?
    };
    doc["model_providers"]
        .as_table_mut()
        .expect("model_providers was initialized")[&provider_id] = Item::Table(provider);
    doc["model_provider"] = value(provider_id);
    if use_official_catalog {
        doc["model_catalog_json"] = value(crate::model_catalog::relative_path());
    } else {
        doc.as_table_mut().remove("model_catalog_json");
    }
    enable_desktop_reasoning_efforts(&mut doc)?;
    ensure_default_service_tier(&mut doc);
    set_model_selection(&mut doc, default_model);
    if let Some(command) = fastctx_command {
        enable_fast_context_tools(&mut doc, command)?;
    }
    if subagent_optimization {
        enable_subagent_optimization(&mut doc)?;
    }
    document_string(&doc)
}

fn enable_subagent_optimization(doc: &mut DocumentMut) -> Result<()> {
    doc.as_table_mut().remove("agents");
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
    let mcp_servers = ensure_root_table(doc, "mcp_servers")?;
    let mut server = Table::new();
    server["command"] = value(command.to_string_lossy().to_string());
    let mut args = Array::new();
    args.push("--codey-fastctx-mcp");
    server["args"] = Item::Value(toml_edit::Value::Array(args));
    server["startup_timeout_sec"] = value(CODEY_FASTCTX_STARTUP_TIMEOUT_SECONDS);
    server["tool_timeout_sec"] = value(120);
    let mut env = Table::new();
    env["FASTCTX_TOKEN_BUDGET"] = value(CODEY_FASTCTX_TOKEN_BUDGET);
    server["env"] = Item::Table(env);
    mcp_servers[CODEY_FASTCTX_SERVER_ID] = Item::Table(server);

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

fn direct_provider_table(profile: &ProviderProfile) -> Result<Table> {
    let base_url = profile.normalized_base_url();
    if base_url.is_empty() {
        anyhow::bail!("第三方线路缺少 API 地址");
    }
    let mut provider = Table::new();
    provider["name"] = value(profile.name.trim());
    provider["base_url"] = value(base_url);
    provider["wire_api"] = value(match profile.protocol {
        RelayProtocol::Responses => "responses",
        RelayProtocol::ChatCompletions => "chat",
    });
    provider["requires_openai_auth"] = value(true);
    if !profile.api_key.trim().is_empty() {
        provider["experimental_bearer_token"] = value(profile.api_key.trim());
    }
    Ok(provider)
}

fn official_provider_table() -> Table {
    let mut provider = Table::new();
    provider["name"] = value("OpenAI (Codey Global)");
    provider["base_url"] = value(CHATGPT_CODEX_BASE_URL);
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(true);
    provider
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
    let temp = parent.join(format!(
        ".{}.codey-tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    write_private_file(&temp, bytes)?;
    match fs::rename(&temp, path) {
        Ok(()) => {}
        Err(error) => {
            #[cfg(windows)]
            {
                if path.exists() {
                    fs::remove_file(path)?;
                    fs::rename(&temp, path)?;
                } else {
                    return Err(error.into());
                }
            }
            #[cfg(not(windows))]
            return Err(error.into());
        }
    }
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

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            true,
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
    fn direct_patch_configures_the_provider_without_a_loopback_endpoint() {
        let result = patch_config(
            "model_provider = \"openai\"\n",
            &direct_profile(RelayProtocol::ChatCompletions),
            "openai",
            false,
        )
        .unwrap();
        assert!(result.contains("base_url = \"https://relay.example/v1\""));
        assert!(result.contains("wire_api = \"chat\""));
        assert!(result.contains("experimental_bearer_token = \"sk-direct\""));
        assert!(!result.contains("127.0.0.1"));
        assert_eq!(
            root_key_string(&result, "model_provider").as_deref(),
            Some(GLOBAL_PROVIDER_ID)
        );
    }

    #[test]
    fn fast_context_tools_register_the_embedded_server_without_overwriting_user_fastctx() {
        let existing = r#"
developer_instructions = "Keep my guidance."
tool_output_token_limit = 16000

[mcp_servers.fastctx]
command = "/custom/fastctx"
args = ["serve", "--enable-shell"]

[features.code_mode]
direct_only_tool_namespaces = ["mcp__existing"]
"#;
        let result = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            true,
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
        assert_eq!(
            document["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["command"].as_str(),
            Some("/Applications/Codey.app/Contents/MacOS/codey")
        );
        assert_eq!(
            document["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["args"][0].as_str(),
            Some("--codey-fastctx-mcp")
        );
        assert_eq!(
            document["mcp_servers"][CODEY_FASTCTX_SERVER_ID]["env"]["FASTCTX_TOKEN_BUDGET"]
                .as_str(),
            Some(CODEY_FASTCTX_TOKEN_BUDGET)
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
                .any(|entry| entry.as_str() == Some(CODEY_FASTCTX_NAMESPACE))
        );
        let guidance = document["developer_instructions"].as_str().unwrap();
        assert!(guidance.starts_with("Keep my guidance."));
        assert!(guidance.contains(CODEY_FASTCTX_GUIDANCE));
        assert!(guidance.contains("always use `mcp__codey_fastctx__read`"));
        assert!(guidance.contains("Do not use cat, sed, rg, grep, find, or recursive ls"));
        assert!(guidance.contains("Use exec only for builds, tests, Git, package managers"));
    }

    #[test]
    fn fast_context_tools_are_idempotent_and_default_the_host_output_limit() {
        let first = patch_config_with_fastctx(
            "",
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            Some(Path::new("/tmp/codey")),
            false,
        )
        .unwrap();
        let second = patch_config_with_fastctx(
            &first,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            true,
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
        let result = patch_config_with_fastctx(
            existing,
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            true,
            None,
            None,
            true,
        )
        .unwrap();
        let document = result.parse::<DocumentMut>().unwrap();
        let multi_agent = document["features"]["multi_agent_v2"].as_table().unwrap();

        assert!(document.get("agents").is_none());
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
        assert!(document.get("agents").is_none());
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
        assert!(!config.contains("[model_providers.openai]"));
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
