pub(crate) const SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

默认由主代理直接处理短而明确、步骤互相依赖或即将修改关键代码/文档的任务；不要为了形式分工而派生。只在独立并行工作、宽范围检索、上下文隔离或独立高风险证据确有收益时使用子代理。不超过 2 个小文件和 3 次本地工具调用的精确任务通常由主代理完成。

纯只读工作最多同时运行 3 个子代理；存在写入型或身份未确认的代理时最多同时运行 2 个。并发限制只约束同时运行数量，不限制后续派发次数。

### 派发

- 直接调用 `agents.spawn_agent`，按任务选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 仅兼容旧配置。`task_name` 只含小写字母、数字和下划线。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景，不复制整段对话，不附加 V1/V2 契约、sidecar、checks 或其他尾行协议。
- 只读角色获得 `files.read`；写入角色获得 `command.execute`、`files.read` 和 `workspace.write`。写入角色暂按当前工作区建立互斥锁；实际文件与网络权限仍由 Codex 原生 sandbox、approval policy、permission profile 和 writable roots 决定。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收；Codey 不再创建逐任务机械验收债或强制验收命令。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，按下一个计划任务的角色重新计算并发上限；存在空余槽位时立即使用新 `task_name` 补位，否则继续等待。所有计划任务均已派发后，继续等待剩余活动 attempt 结算。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 成功的 `agents.interrupt_agent` 会永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账：原代理存在则等待或消费结果，不存在则由根代理接管。只有任务范围实质改变时才用全新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界的 pending-init、超时和 Stop 恢复路径收敛。
"#;

/// Only the current text is recognised. Codey supports upgrades from the
/// previous two releases only, and both shipped this exact text; older
/// guidance is left untouched instead of being migrated.
pub(crate) const SUBAGENT_GUIDANCE_VERSIONS: &[&str] = &[SUBAGENT_GUIDANCE];

pub(crate) const ROOT_AGENT_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools are direct commentary \
tools; never call them through `functions.exec`. Dispatch up to the current concurrency limit from the \
planned independent work before the first wait. While any attempt is active, use only the relevant \
`agents.spawn_agent`, `agents.send_message`, `agents.followup_task`, `agents.interrupt_agent`, \
`agents.list_agents`, or `agents.wait_agent`. After a terminal or successfully fenced update, recompute the \
role-aware concurrency limit; if it exposes a slot, immediately use `agents.spawn_agent` with a new \
`task_name` for the next planned, unspawned task; \
otherwise return to `agents.wait_agent` with `timeout_ms: 30000`. `MESSAGE` and mailbox updates are not \
completion. Use `followup_task` only for a bound nonterminal attempt. If \
`CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not retry or wait for that target; take \
over or use a fresh `task_name` for a materially changed task. Treat `FINAL_ANSWER`, `task_complete`, \
`completed`, `errored`, `error`, `failed`, `shutdown`, and `not_found` as terminal. A successful root \
interrupt permanently abandons and fences that attempt, settles it for the lifecycle ledger, and makes \
later active-looking provider state stale; do not wait for or follow up that target. If a wait times out or \
lacks per-agent terminal details, call unfiltered `agents.list_agents` before waiting again. Continue until \
all planned work has been spawned and every attempt is terminal or fenced. Then the root agent validates \
the combined result and either continues the work or finishes. While an attempt is active, Codey's gate \
blocks non-collaboration tools and Stop. If collaboration tools are unavailable, do not loop on an \
unregistered tool.";

pub(crate) const ROOT_AGENT_MULTI_AGENT_MODE_HINT: &str = "Proactive multi-agent delegation is \
active. Any earlier instruction requiring an explicit user request before spawning sub-agents no \
longer applies. Delegate only when independent parallel work, context isolation, or specialized evidence \
materially helps; keep short, sequential, or low-benefit work with the root. There is no fixed spawn \
budget: up to three concurrent agents are allowed only when all are verified read-only, otherwise the \
limit is two. `CODEY_SUBAGENT_CONCURRENCY_LIMIT` means wait for a slot, not failure; when any child settles, \
recompute the role-aware limit and fill a slot from the remaining planned independent work when allowed. If an active child \
cannot decrypt its task body, use `agents.send_message` exactly once to restate the complete task; do not \
interrupt or respawn it. If that fails, take over. After all attempts settle, validate their combined result \
before continuing or finishing. If every spawn fails, take over. On `CODEY_SUBAGENT_DUPLICATE_TASK_ID`, call \
unfiltered `agents.list_agents` once: wait for the original if present, otherwise take over. Only a \
materially changed task may retry once with a fresh `task_name`. This \
mode remains active until a later multi-agent mode developer message changes it.";

/// Only the current text is recognised. Codey supports upgrades from the
/// previous two releases only, and both shipped this exact text; older
/// guidance is left untouched instead of being migrated.
pub(crate) const ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS: &[&str] =
    &[ROOT_AGENT_COLLABORATION_USAGE_HINT];

pub(crate) const DEFAULT_AGENT_CONFIG: &str = r#####"name = "default"

description = "General-purpose exploration subagent using the configured default model and reasoning effort."
sandbox_mode = "read-only"

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
- 每次工具调用都必须推进任务本身。进度、道歉、自我提醒和纠错写在回复中；发现工具用错时直接改用正确工具，不要为此额外执行诊断或播报命令。
- 首行写 `status: completed | partial | blocked`；只保留会影响决策的结论、最多 5 条关键证据和明确 gaps。
"""

[features]
image_generation = false
"#####;

pub(crate) const QUICK_SCAN_AGENT_CONFIG: &str = r#####"name = "codey_quick_scan"

description = "Read-only fast lookup for exact locations, repetitive checks, and low-risk factual retrieval."
sandbox_mode = "read-only"

developer_instructions = """
你是快速定位子代理。只做只读、范围明确、低风险的定位与事实检索，不修改任何文件，不做方案取舍，也不派生其他子代理。
优先返回最短可核验证据：确切路径、`file:line`、符号名、匹配数量和必要的关键原文。任务超出小范围快速检索时，明确说明应改派深度检索或视觉分析角色。
你的回复直接供主代理使用：密而不水，区分事实与推断，不寒暄、不复述过程。
首行写 `status: completed | partial | blocked`；最多保留 5 条会影响决策的关键证据，并明确未覆盖范围。
"""

[features]
image_generation = false
"#####;

pub(crate) const DEEP_RESEARCH_AGENT_CONFIG: &str = r#####"name = "codey_deep_research"

description = "Read-only broad research across code, logs, and documents for synthesis and architecture exploration."
sandbox_mode = "read-only"

developer_instructions = """
你是深度检索子代理。负责跨文件、跨目录的代码、日志和文档检索、归纳与架构探索；不修改任何文件，不做最终方案取舍，也不派生其他子代理。
覆盖任务给定范围，返回符号关系、关键路径、`file:line` 和必要原文。把已确认事实、推断、缺口与矛盾分开，保留足够证据供主代理低成本抽查。
你的输出直接喂给主代理：结构紧凑、信息密集，不写面向最终用户的包装文字。
首行写 `status: completed | partial | blocked`；只返回会影响决策的结论、最多 5 条关键证据和明确 gaps。
"""

[features]
image_generation = false
"#####;

pub(crate) const VISUAL_ANALYSIS_AGENT_CONFIG: &str = r#####"name = "codey_visual_analysis"

description = "Read-only visual analysis for screenshots, pages, GUI states, PDFs, and independent evidence review."
sandbox_mode = "read-only"

developer_instructions = """
你是视觉分析子代理。负责截图、页面、GUI、PDF 和渲染结果的只读观察，也可承担需要视觉证据的复杂探索与独立核验；不修改文件，不做最终方案取舍，也不派生其他子代理。
先读取或捕获必要视觉证据，再报告可见事实、位置关系、状态差异和可复核出处；推断必须单独标注。不要仅凭文件名或代码猜测视觉结果。
你的输出直接供主代理决策，保持精炼、具体、可核验。
首行写 `status: completed | partial | blocked`；只返回会影响决策的可见事实、最多 5 条关键证据和明确 gaps。
"""

[features]
image_generation = false
"#####;

pub(crate) const WORKER_AGENT_CONFIG: &str = r#####"name = "codey_worker"

description = "Writable implementation for bounded, reversible, testable, low-to-medium complexity non-visual tasks."
sandbox_mode = "workspace-write"

developer_instructions = """
你是代码实施子代理。只处理主代理明确授权、边界清晰、可回滚且可测试的低到中等复杂度非视觉实现；不要扩大范围，不做跨模块架构取舍，也不派生其他子代理。
修改前读取将要编辑的确切代码，保留并适配其他人的并行改动。仅当契约显式授予 `command.execute` 时运行命令验证；实际文件与命令访问继续受继承的 Codex 原生权限约束。否则列出需要主代理执行的检查。返回修改文件、关键位置、已取得的验证证据和仍存风险。
遇到需要产品选择、破坏性操作或范围不明确时停止修改，把阻塞点交回主代理。
首行写 `status: completed | partial | blocked`；紧凑列出改动、确定性验证结果和未完成项，不回传冗长日志。
"""

[features]
image_generation = false
"#####;

pub(crate) const VISUAL_WORKER_AGENT_CONFIG: &str = r#####"name = "codey_visual_worker"

description = "Writable implementation for pages, GUI, PDFs, and tasks that require visual evidence or render verification."
sandbox_mode = "workspace-write"

developer_instructions = """
你是视觉实施子代理。只处理主代理明确授权、边界清晰且需要截图、页面、GUI、PDF 或渲染证据的低到中等复杂度实现；不要扩大范围，不做架构取舍，也不派生其他子代理。
修改前读取确切代码与视觉基线，修改后通过已授权的渲染/截图工具核验；仅当契约显式授予 `command.execute` 时运行通用命令，实际文件与命令访问继续受继承的 Codex 原生权限约束。报告修改文件、关键位置、视觉证据、验证结果和仍存风险。
保留并适配其他人的并行改动；遇到需要产品选择、破坏性操作或范围不明确时停止并交回主代理。
首行写 `status: completed | partial | blocked`；紧凑列出改动、视觉与确定性验证结果和未完成项，不回传冗长日志。
"""

[features]
image_generation = false
"#####;

pub(crate) const READ_ONLY_AGENT_WRITE_GUARD: &str = "\
当前任务类型是只读子代理，只能检查、搜索、分析和回报。不要调用 `replace`、`apply_patch`、\
文件写入命令或任何会创建、修改、删除、移动文件及改变外部状态的工具。即使任务正文要求写入，\
也不要尝试或重试；请停止实施，把需要修改的内容和证据交回主代理，由主代理完成写入。";

pub(crate) const NO_WRITABLE_SUBAGENT_GUIDANCE: &str = "\
本次运行没有启用 `codey_worker` 或 `codey_visual_worker`，因此没有可写子代理。所有创建、修改、\
删除、移动文件或其他会改变状态的工作都由主代理直接完成；只读子代理只能承担检索、分析和证据\
收集。不得把写入任务改派给 `default` 或任何只读角色，也不得要求它们尝试 `replace`、\
`apply_patch` 或其他写入工具。";

pub(crate) fn subagent_source_config(role: &str) -> Option<&'static str> {
    match role {
        "codey_quick_scan" => Some(QUICK_SCAN_AGENT_CONFIG),
        "codey_deep_research" => Some(DEEP_RESEARCH_AGENT_CONFIG),
        "codey_visual_analysis" => Some(VISUAL_ANALYSIS_AGENT_CONFIG),
        "codey_worker" => Some(WORKER_AGENT_CONFIG),
        "codey_visual_worker" => Some(VISUAL_WORKER_AGENT_CONFIG),
        "default" => Some(DEFAULT_AGENT_CONFIG),
        _ => None,
    }
}

pub(crate) const CODEY_FASTCTX_GUIDANCE: &str = "Codey FastCtx context tools are enabled as direct \
tools. Use `mcp__codey_fastctx__inspect_local_file` for focused inspection, \
`mcp__codey_fastctx__grep` for search, `mcp__codey_fastctx__glob` for discovery, and \
`mcp__codey_fastctx__replace` only for deterministic replacement. Batch 2-32 known text files or ranges \
per inspect call; limit large files to needed ranges. A top-level `limit` applies to entries without one. \
Pass plain absolute filesystem paths; convert local URIs and Windows paths to a drive-letter path such as \
`E:/repo/file.ts`. Start broad grep with `files_with_matches`, then request minimal `content`; use summary \
or count only for totals. Glob with `filter_mode=ignore`, stable sorting, and `output_mode=details` only \
when metadata matters. Run replace as dry-run first with `max_replacements`, then inspect and test; never \
transparently retry a write after transport failure. Follow every Complete or Partial continuation without \
parallel page speculation. FastCtx is a direct-only tool namespace, not an MCP Resources server or \
code-mode aggregate; call tools directly and use `tool_search` when deferred. Use terminal commands for \
builds, tests, Git, package managers, advanced shell/streaming operations, unsupported metadata, or after \
the applicable FastCtx tool is unavailable or fails. Use CodeGraph only for semantic symbols and call \
paths. Every tool call must advance the task; put progress and corrections in commentary.";

/// Only the current text is recognised. Codey supports upgrades from the
/// previous two releases only, and both shipped this exact text; older
/// guidance is left untouched instead of being migrated.
pub(crate) const CODEY_FASTCTX_GUIDANCE_VERSIONS: &[&str] = &[CODEY_FASTCTX_GUIDANCE];

const DEFAULT_FASTCTX_TOOL_NAMESPACE: &str = "mcp__codey_fastctx";

pub(crate) fn codey_fastctx_guidance_for_namespace(namespace: &str) -> String {
    CODEY_FASTCTX_GUIDANCE.replace(DEFAULT_FASTCTX_TOOL_NAMESPACE, namespace)
}

#[cfg(test)]
pub(crate) fn default_agent_config_with_fastctx_guidance(namespace: Option<&str>) -> String {
    let Some(namespace) = namespace else {
        return DEFAULT_AGENT_CONFIG.to_string();
    };
    let guidance = codey_fastctx_guidance_for_namespace(namespace);
    let marker = "\n\"\"\"\n\n[features]\n";
    let replacement = format!("\n\n{guidance}\n\"\"\"\n\n[features]\n");
    DEFAULT_AGENT_CONFIG.replacen(marker, &replacement, 1)
}

pub(crate) fn codey_fastctx_guidance_blocks(current: &str) -> Vec<String> {
    fastctx_guidance_blocks(current, CODEY_FASTCTX_GUIDANCE_VERSIONS)
}

fn fastctx_guidance_blocks(current: &str, versions: &[&str]) -> Vec<String> {
    let mut blocks = Vec::new();
    for &guidance in versions {
        if current.contains(guidance) {
            blocks.push(guidance.to_string());
        }

        let Some(prefix_end) = guidance.find(DEFAULT_FASTCTX_TOOL_NAMESPACE) else {
            continue;
        };
        let prefix = &guidance[..prefix_end];
        for (start, _) in current.match_indices(prefix) {
            let Some(dynamic_guidance) =
                dynamic_codey_fastctx_guidance_at(current, start, guidance)
            else {
                continue;
            };
            if !blocks.iter().any(|block| block == &dynamic_guidance) {
                blocks.push(dynamic_guidance);
            }
        }
    }
    blocks
}

fn dynamic_codey_fastctx_guidance_at(
    current: &str,
    start: usize,
    guidance_template: &str,
) -> Option<String> {
    let prefix_end = guidance_template.find(DEFAULT_FASTCTX_TOOL_NAMESPACE)?;
    let after_template_namespace =
        guidance_template.get(prefix_end + DEFAULT_FASTCTX_TOOL_NAMESPACE.len()..)?;
    let tool_suffix_end = after_template_namespace.find('`')?;
    let tool_suffix = &after_template_namespace[..=tool_suffix_end];
    let after_prefix = current.get(start + prefix_end..)?;
    let namespace_end = after_prefix.find(tool_suffix)?;
    let namespace = &after_prefix[..namespace_end];
    if namespace.is_empty()
        || namespace.contains('`')
        || namespace.contains('\n')
        || namespace.contains('\r')
        || !namespace.starts_with("mcp__")
    {
        return None;
    }
    let guidance = guidance_template.replace(DEFAULT_FASTCTX_TOOL_NAMESPACE, namespace);
    current[start..].starts_with(&guidance).then_some(guidance)
}

pub(crate) fn append_root_agent_collaboration_usage_hint(existing: &str) -> String {
    let current_is_present =
        guidance_paragraph_start(existing, ROOT_AGENT_COLLABORATION_USAGE_HINT).is_some();
    let mut updated = existing.to_string();
    for &guidance in ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS {
        if current_is_present && guidance == ROOT_AGENT_COLLABORATION_USAGE_HINT {
            continue;
        }
        while let Some(without_guidance) = remove_owned_guidance_paragraph(&updated, guidance) {
            updated = without_guidance;
        }
    }
    if current_is_present {
        return updated;
    }
    let mut updated = updated.trim_end().to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(ROOT_AGENT_COLLABORATION_USAGE_HINT);
    updated
}

pub(crate) fn remove_subagent_guidance(current: &str) -> Option<String> {
    let mut restored = current.to_string();
    let mut changed = false;
    for &guidance in SUBAGENT_GUIDANCE_VERSIONS {
        while let Some(without_guidance) = remove_owned_guidance_block(&restored, guidance) {
            restored = without_guidance;
            changed = true;
        }
    }
    changed.then_some(restored)
}

pub(crate) fn remove_codey_fastctx_guidance(current: &str) -> Option<String> {
    remove_fastctx_guidance_blocks(current, codey_fastctx_guidance_blocks(current))
}

fn remove_fastctx_guidance_blocks(current: &str, guidance_blocks: Vec<String>) -> Option<String> {
    let mut restored = current.to_string();
    let mut changed = false;
    for guidance in guidance_blocks {
        while let Some(without_guidance) = remove_owned_guidance_paragraph(&restored, &guidance) {
            restored = without_guidance;
            changed = true;
        }
    }
    changed.then_some(restored)
}

pub(crate) fn remove_owned_guidance_block(current: &str, guidance: &str) -> Option<String> {
    let guidance_start = current.find(guidance)?;
    Some(remove_guidance_at(current, guidance_start, guidance.len()))
}

pub(crate) fn remove_owned_guidance_paragraph(current: &str, guidance: &str) -> Option<String> {
    let guidance_start = guidance_paragraph_start(current, guidance)?;
    Some(remove_guidance_at(current, guidance_start, guidance.len()))
}

fn guidance_paragraph_start(current: &str, guidance: &str) -> Option<usize> {
    current.match_indices(guidance).find_map(|(start, _)| {
        let end = start + guidance.len();
        let starts_paragraph = start == 0 || current[..start].ends_with("\n\n");
        let ends_paragraph = end == current.len() || current[end..].starts_with("\n\n");
        (starts_paragraph && ends_paragraph).then_some(start)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fastctx_guidance_encodes_the_direct_and_cost_efficient_tool_contract() {
        assert_eq!(
            codey_fastctx_guidance_for_namespace("mcp__codey_fastctx"),
            CODEY_FASTCTX_GUIDANCE
        );
        assert!(CODEY_FASTCTX_GUIDANCE.contains("enabled as direct tools"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("Batch 2-32 known text files or ranges"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("top-level `limit`"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("grep with `files_with_matches`"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("`filter_mode=ignore`"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("`output_mode=details`"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("Run replace as dry-run first"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("never transparently retry a write"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("Complete or Partial continuation"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("Use CodeGraph only for semantic symbols"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("call tools directly"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("a direct-only tool namespace"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("use `tool_search`"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("convert local URIs"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("plain absolute filesystem paths"));
        assert!(CODEY_FASTCTX_GUIDANCE.contains("drive-letter path such as `E:/repo/file.ts`"));
        assert!(!CODEY_FASTCTX_GUIDANCE.contains("inspect `ALL_TOOLS`"));
        assert!(!CODEY_FASTCTX_GUIDANCE.contains("functions.exec"));
        for resource_helper in [
            "list_mcp_resources",
            "list_mcp_resource_templates",
            "read_mcp_resource",
            "resources/*",
        ] {
            assert!(!CODEY_FASTCTX_GUIDANCE.contains(resource_helper));
        }
        assert!(CODEY_FASTCTX_GUIDANCE.contains("Every tool call must advance the task"));
        assert!(!CODEY_FASTCTX_GUIDANCE.contains("no separate tool discovery is needed"));
        assert!(!CODEY_FASTCTX_GUIDANCE.contains("Write-Output"));
        assert!(!CODEY_FASTCTX_GUIDANCE.contains("file:///"));
        assert!(!CODEY_FASTCTX_GUIDANCE.contains("__read"));
    }

    #[test]
    fn default_agent_config_can_include_the_fastctx_namespace_guidance() {
        let config = default_agent_config_with_fastctx_guidance(Some("mcp__fastctx"));

        assert!(config.contains("`mcp__fastctx__inspect_local_file`"));
        assert!(config.contains("`mcp__fastctx__grep`"));
        assert!(config.contains("`mcp__fastctx__glob`"));
        assert!(config.contains("`mcp__fastctx__replace`"));
        assert!(config.contains("Batch 2-32 known text files or ranges"));
        assert!(config.contains("Use CodeGraph only for semantic symbols"));
        assert!(config.contains("a direct-only tool namespace"));
        assert!(config.contains("use `tool_search`"));
        assert!(!config.contains("inspect `ALL_TOOLS`"));
        assert!(!config.contains("list_mcp_resources"));
        assert!(!config.contains("read_mcp_resource"));
        assert!(config.contains("put progress and corrections in commentary"));
        assert!(!config.contains("no separate tool discovery is needed"));
        assert!(!config.contains("Write-Output"));
        assert!(!config.contains("mcp__codey_fastctx"));
        assert!(config.contains("[features]"));
        assert!(config.ends_with("image_generation = false\n"));
    }

    #[test]
    fn default_agent_never_uses_terminal_commands_as_narration() {
        assert!(DEFAULT_AGENT_CONFIG.contains("不要派生、调用或者请求新的子代理"));
        assert!(DEFAULT_AGENT_CONFIG.contains("每次工具调用都必须推进任务本身"));
        assert!(DEFAULT_AGENT_CONFIG.contains("进度、道歉、自我提醒和纠错写在回复中"));
        assert!(DEFAULT_AGENT_CONFIG.contains("直接改用正确工具"));
        assert!(!DEFAULT_AGENT_CONFIG.contains("Write-Output"));
        assert!(!DEFAULT_AGENT_CONFIG.contains("Write-Error"));
    }

    #[test]
    fn root_agent_usage_hint_routes_collaboration_tools_directly() {
        let custom = "Preserve my root usage hint.";
        let combined = append_root_agent_collaboration_usage_hint(custom);

        assert!(combined.contains(custom));
        assert!(combined.contains("`agents.spawn_agent`"));
        assert!(combined.contains("before the first wait"));
        assert!(combined.contains("direct commentary tools"));
        assert!(combined.contains("`timeout_ms: 30000`"));
        assert!(combined.contains("mailbox updates are not completion"));
        assert!(combined.contains("`MESSAGE`"));
        assert!(combined.contains("Dispatch up to the current concurrency limit"));
        assert!(combined.contains("`agents.send_message`"));
        assert!(combined.contains("Use `followup_task` only for"));
        assert!(combined.contains("`CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT`"));
        assert!(combined.contains("use a fresh `task_name`"));
        assert!(combined.contains("`FINAL_ANSWER`"));
        assert!(combined.contains("`task_complete`"));
        assert!(combined.contains("`errored`"));
        assert!(combined.contains("`error`"));
        assert!(combined.contains("`failed`"));
        assert!(combined.contains("`shutdown`"));
        assert!(combined.contains("`not_found`"));
        assert!(combined.contains("successful root interrupt permanently abandons and fences"));
        assert!(combined.contains("settles it for the lifecycle ledger"));
        assert!(combined.contains("do not wait for or follow up that target"));
        assert!(combined.contains("active-looking provider state stale"));
        assert!(combined.contains("terminal or fenced"));
        assert!(combined.contains("unfiltered `agents.list_agents`"));
        assert!(combined.contains("recompute the role-aware concurrency limit"));
        assert!(combined.contains("next planned, unspawned task"));
        assert!(combined.contains("all planned work has been spawned"));
        assert!(combined.contains("do not loop on an unregistered tool"));
        assert!(combined.contains("never call them through `functions.exec`"));
        assert!(!combined.contains("Write-Output"));
        assert!(!combined.contains("Write-Error"));
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&combined),
            combined
        );
        let current_before_user =
            format!("{ROOT_AGENT_COLLABORATION_USAGE_HINT}\n\nPreserve this position.");
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&current_before_user),
            current_before_user
        );
    }

    #[test]
    fn multi_agent_mode_hint_uses_role_aware_concurrency_without_spawn_budgets() {
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("There is no fixed spawn budget"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("up to three concurrent agents"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("limit is two"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("`CODEY_SUBAGENT_CONCURRENCY_LIMIT`"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("recompute the role-aware limit"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("cannot decrypt its task body"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("`agents.send_message` exactly once"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("do not interrupt or respawn it"));
        assert!(
            !ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("CODEY_SUBAGENT_BATCH_BUDGET_EXHAUSTED")
        );
        assert!(!ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("CODEY_SUBAGENT_TURN_BUDGET_EXHAUSTED"));
    }

    #[test]
    fn subagent_guidance_prefers_direct_work_until_delegation_has_clear_value() {
        assert!(SUBAGENT_GUIDANCE.contains("默认由主代理直接处理"));
        assert!(SUBAGENT_GUIDANCE.contains("不超过 2 个小文件和 3 次本地工具调用"));
        assert!(SUBAGENT_GUIDANCE.contains("直接调用 `agents.spawn_agent`"));
        assert!(SUBAGENT_GUIDANCE.contains("唯一任务胶囊"));
        assert!(SUBAGENT_GUIDANCE.contains("纯只读工作最多同时运行 3 个子代理"));
        assert!(SUBAGENT_GUIDANCE.contains("最多同时运行 2 个"));
        assert!(SUBAGENT_GUIDANCE.contains("按下一个计划任务的角色重新计算并发上限"));
        assert!(SUBAGENT_GUIDANCE.contains("普通本地工作和 Stop 仍受生命周期门禁限制"));
        assert!(SUBAGENT_GUIDANCE.contains("status: completed | partial | blocked"));
        assert!(SUBAGENT_GUIDANCE.contains("多代理证据冲突时比较出处"));
        assert!(SUBAGENT_GUIDANCE.contains("根代理结合用户要求"));
        assert!(SUBAGENT_GUIDANCE.contains("Codey 不再创建逐任务机械验收债"));
        assert!(SUBAGENT_GUIDANCE.contains("成功的 `agents.interrupt_agent` 会永久 fence"));
        assert!(SUBAGENT_GUIDANCE.contains("`files.read`"));
        assert!(SUBAGENT_GUIDANCE.contains("`workspace.write`"));
        assert!(SUBAGENT_GUIDANCE.contains("Codex 原生 sandbox"));
        assert!(!SUBAGENT_GUIDANCE.contains("CODEY_DELEGATION_V2="));
        assert!(!SUBAGENT_GUIDANCE.contains("prepare_delegation"));
        assert!(!SUBAGENT_GUIDANCE.contains("resolve_batch"));
        assert!(!SUBAGENT_GUIDANCE.contains("# codey-accept:"));
        assert!(SUBAGENT_GUIDANCE.contains("`completed`、`errored`、`error`、`failed`"));
    }

    #[test]
    fn every_task_role_has_a_named_editable_source_template() {
        for (role, writable) in [
            ("codey_quick_scan", false),
            ("codey_deep_research", false),
            ("codey_visual_analysis", false),
            ("codey_worker", true),
            ("codey_visual_worker", true),
            ("default", false),
        ] {
            let source = subagent_source_config(role).unwrap();
            assert!(source.contains(&format!("name = \"{role}\"")));
            assert!(source.contains("description = \""));
            let expected = if writable {
                "sandbox_mode = \"workspace-write\""
            } else {
                "sandbox_mode = \"read-only\""
            };
            assert!(source.contains(expected), "{role}");
        }
    }

    #[test]
    fn fastctx_guidance_cleanup_removes_every_codey_owned_version() {
        let user_server_guidance = CODEY_FASTCTX_GUIDANCE_VERSIONS
            .iter()
            .map(|guidance| guidance.replace(DEFAULT_FASTCTX_TOOL_NAMESPACE, "mcp__fastctx"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let configured = format!(
            "User guidance.\n\n{}\n\n{user_server_guidance}\n\nConcurrent guidance.",
            CODEY_FASTCTX_GUIDANCE_VERSIONS.join("\n\n"),
        );

        assert_eq!(
            remove_codey_fastctx_guidance(&configured).as_deref(),
            Some("User guidance.\n\nConcurrent guidance.")
        );
    }

    #[test]
    fn fastctx_guidance_blocks_detect_user_fastctx_namespaces() {
        let user_server_guidance = CODEY_FASTCTX_GUIDANCE_VERSIONS
            .iter()
            .map(|guidance| guidance.replace(DEFAULT_FASTCTX_TOOL_NAMESPACE, "mcp__context_tools"))
            .collect::<Vec<_>>();
        let configured = format!("Prefix\n\n{}\n\nSuffix", user_server_guidance.join("\n\n"));

        assert_eq!(
            codey_fastctx_guidance_blocks(&configured),
            user_server_guidance
        );
    }
}
