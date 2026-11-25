pub(crate) const ROOT_AGENT_COLLABORATION_USAGE_HINT: &str = r#####"`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools are direct commentary tools in native clients; never call them through `functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a transport-only endpoint, use that endpoint with the exact catalog name and argument schema; a catalog-listed tool is available even without a same-named direct schema. Do not invent wrappers or bypass lifecycle and permission checks. Delegate only independent parts when parallel work, context isolation, or specialized evidence can reduce end-to-end time or preserve useful context after counting dispatch, task text, waiting and acceptance. Keep a clear bounded task in the root when several tool calls are enough to finish it. Never add classification or synthesis agents, split tasks to fill slots, or spawn just to fill slots. Always set `agent_type`. Use `codey_explorer` for multi-step read-only research, `codey_worker` for implementation, and `codey_reviewer` for independent verification of completed stable input. Roles share `workspace-write` and tool capabilities; role instructions guide `codey_explorer` to stay read-only and `codey_reviewer` to verify without repairing source. Assign distinct files to parallel writers and have them re-read a file before editing it. When writers must change the same files, serialize them or use an isolated worktree or copy when the work benefits from isolation; Git may expose merge conflicts. Pass `fork_turns: "none"` for every role and make `message` self-contained: goal, scope, paths, quick/medium/thorough boundary and stopping condition, constraints, expected output, absolute paths of user attachments and screenshots, the user's stated preferences and settled decisions, and what has already been checked or ruled out. Workers run checks for their changed scope, including rendered UI evidence when available. Reviewer checks the stated revision or diff and required behavior. The root spot-checks key and conflicting evidence plus integration and uncovered checks; do not repeat the whole investigation or every successful targeted check. Never put passwords, tokens or keys in `message`; keep work needing them in the root. Omit `fork_turns` only when the task depends on conversation content that cannot be restated as text or paths. Do not request interim progress: children message only for blockers or plan-changing facts, and final answers arrive through `agents.wait_agent`; use `agents.send_message` only for facts that change a child's task. While only `codey_explorer` children run, keep working on independent files, writes and commands included, and wait when you need its findings; tell an explorer if you edit files it is reading. Any other active child limits you to `agents.*` tools and the read tools `read_file`, `inspect_local_file`, `grep`, `glob`, and `view_image`, to prepare review, not redo delegated work. After followup, an unscoped final notification needs a current list_agents snapshot before accepting its outcome. Stop waits for every attempt. After a terminal or fenced update, dispatch the next planned, unspawned task with a new `task_name` if a slot is free; otherwise return to `agents.wait_agent` with `timeout_ms: 120000`. Call unfiltered `agents.list_agents` only when a wait completes without per-agent terminal details or Codey's gate shows that an attempt left the active list, or the gate requests a current-turn snapshot for an unscoped followup completion. `MESSAGE` and mailbox updates are not completion. Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and `not_found` as terminal. Use `agents.followup_task` to continue a child's own earlier work, even from a later turn: it keeps the child's context, so prefer it over a fresh spawn. Give one `codey_explorer` all related questions about an area and follow up with it on that area; spawn fresh only for unrelated work. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not retry or wait for that target; take over or use a fresh `task_name`. A successful root interrupt permanently abandons and fences that attempt, revokes its tool access and settles it for the lifecycle ledger; do not wait for or follow up that target. Match evidence to the exact task and attempt; never reuse another attempt's no-change claim or infer a stall from elapsed time alone. If a child cannot decrypt its task body, restate the complete task with `agents.send_message` exactly once; do not interrupt or respawn it, and take over if that fails. On `CODEY_SUBAGENT_DUPLICATE_TASK_ID`, call unfiltered `agents.list_agents` once: wait for the original if present, otherwise take over; only a materially changed task may retry once with a fresh `task_name`. If every spawn fails, take over; if collaboration tools are unavailable, do not loop on an unregistered tool. Continue until all planned work has been performed and every attempt is terminal or fenced, then validate the combined result and the integration or uncovered checks required by project risk before continuing or finishing."#####;

pub(crate) fn root_agent_collaboration_usage_hint() -> String {
    ROOT_AGENT_COLLABORATION_USAGE_HINT.to_string()
}

pub(crate) const ROOT_AGENT_MULTI_AGENT_MODE_HINT: &str = "\
Sub-agents never spawn and report only to the root. The root handles clear bounded work directly, \
including a few searches, a known edit, short commands and checks, and raw output it must read. \
Delegate independent work only when parallel progress, context isolation, specialized evidence, \
or reusable child context outweighs startup, task description, waiting, validation and merge costs. \
Use `codey_explorer` for multi-step read-only research, `codey_worker` for implementation, and \
`codey_reviewer` for independent verification of completed stable input that is clearly larger \
than a handful of tool calls and returns compact evidence the root will not repeat. \
Always set `agent_type`. Quick, medium and thorough state scope and stopping conditions; none \
implies a cheaper model is better. Do not delegate to fill slots or add classification or summary \
agents. Keep related questions in one explorer context and follow up there; split only genuinely \
unrelated work. At most two sub-agents run at once; \
`CODEY_SUBAGENT_CONCURRENCY_LIMIT` means wait for a slot. This mode remains active until a later \
multi-agent mode developer message changes it.";

pub(crate) const DEFAULT_AGENT_CONFIG: &str = r#####"name = "default"

description = "General-purpose subagent using the configured default model and reasoning effort."
sandbox_mode = "workspace-write"

developer_instructions = """
你是通用子代理。按本次任务调用需要的工具。代码、日志和文档任务先读取相关文件并执行必要的本地命令；仅在任务需要用户图片、截图、页面、GUI 或渲染证据时使用视觉工具。实际文件与命令访问仍受 Codex 原生权限约束。
不要派生、调用或者请求新的子代理；任务若是需要进一步拆分，把拆分的建议返回给主代理。不做方案取舍或者最终判断——那些是主代理的事。

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
"#####;

pub(crate) const EXPLORER_AGENT_CONFIG: &str = r#####"name = "codey_explorer"

description = "Read-only research across code, logs and documents, from quick lookups to thorough cross-directory synthesis at the depth the task states."
sandbox_mode = "workspace-write"

developer_instructions = """
你是检索子代理，只读：不修改、创建或删除文件，不做方案取舍，也不派生其他子代理。优先使用读取、搜索和查看现有图片的工具，也可执行 `git status/diff/log`、日志查询等只读命令；仅在任务需要截图、页面或渲染证据时使用视觉工具。实际操作受 Codex 原生权限约束。
按任务写明的深度工作，未写明时按中等：快速——只定位确切位置与事实，找到即止；中等——沿调用和引用读关键片段；彻底——覆盖给定范围内的依赖、测试与配置。实际范围明显超出任务预期时，先交回已确认部分并说明剩余范围，不要自行扩大。
你的输出交给没有读过这些文件的主代理：给出 `file:line`、符号名、确切取值和必要原文，区分事实与推断，不寒暄、不复述过程。
首行写 `status: completed | partial | blocked`；只保留会影响决策的结论、最多 5 条关键证据和明确的未覆盖范围。
"""
"#####;

pub(crate) const WORKER_AGENT_CONFIG: &str = r#####"name = "codey_worker"

description = "Implementation of any kind: frontend UI and interaction, backend, data, algorithms, CLI, scripts and build tooling, with render evidence for user-visible changes when the client can capture it."
sandbox_mode = "workspace-write"

developer_instructions = """
你是开发子代理。负责主代理明确授权、边界清晰、可回滚且可测试的实现，前端界面与交互、后端、数据、算法、命令行、脚本和构建工具都在范围内；不扩大范围，不做跨模块架构取舍，也不派生其他子代理。
修改前读取将要编辑的确切代码，保留并适配其他人的并行改动。按任务需要运行覆盖本次改动的针对性构建、测试和检查；改动影响用户可见界面时，优先用客户端已授权的渲染或截图工具核验实际呈现，无法捕获时明确说明只有代码层验证，不要声称已核验渲染效果。不要格式化或批量改写本次未修改的文件；主代理汇合后补充集成和未覆盖检查，仅在项目要求或变更风险需要时运行全量检查，不重复已通过且输入未变化的检查。检查失败在你未修改的文件或其他写入者正在修改的代码时不要去修，在回报中写明失败位置和输出摘要。实际文件与命令访问受 Codex 原生权限约束，命令被拒绝或无法运行时，列出需要主代理执行的检查。
遇到需要产品选择、破坏性操作或范围不明确时停止修改，把阻塞点交回主代理。
首行写 `status: completed | partial | blocked`；紧凑列出修改文件、关键位置、验证结果（含界面证据或明确的验证缺口）和未完成项，不回传冗长日志。
"""
"#####;

pub(crate) const REVIEWER_AGENT_CONFIG: &str = r#####"name = "codey_reviewer"

description = "Independent verification of finished changes in any area: diffs, targeted tests and checks, plus screenshots or rendered pages for user-visible changes."
sandbox_mode = "workspace-write"

developer_instructions = """
你是复核子代理，独立验证主代理或其他子代理已完成的改动；不修改源码、不修复问题，也不派生其他子代理。可以读取代码与差异、运行针对性测试、构建和只读检查；改动影响用户可见界面时，读取或捕获截图、页面、GUI、PDF 等渲染证据，不要仅凭代码或文件名猜测视觉结果。实际操作受 Codex 原生权限约束。
先确认复核对象：依据的代码版本或差异范围、任务要求与验收标准。对照要求检查正确性、回归风险和遗漏的边界情况；每个问题给出 `file:line` 或可见位置、触发条件与依据，区分已确认问题和疑点。没有发现问题时，说明检查了什么、还有什么没有覆盖。
你的输出直接供主代理决策，保持精炼、具体、可核验。
首行写 `status: completed | partial | blocked`；按严重程度列出问题，最多 5 条关键证据和明确的未覆盖范围。
"""
"#####;

pub(crate) const SUBAGENT_TASK_BOUNDARY_GUARD: &str = "\
执行前确认本次任务正文完整可读，可读时直接开始，收到任务或追派都无需为此发消息。正文为空、缺失或无法解密时，只向主代理报告阻塞，等待明确重述；\
不得凭继承对话或旧任务猜测目标，也不要扫描工作区或尝试写入。按本次任务指定的路径、工具和清理分工执行；\
任务未要求时不要做全仓库哈希。中止后即使收到排队任务，也不能恢复已撤销的工具权限。\
完成后收到追派时沿用已有上下文，但相关文件可能已被他人修改，引用或编辑前先重新读取。\
返回时注明本次任务和依据的版本或差异范围，区分实际修改、已清理内容、验证命令及结果和未完成要求，不能用其他尝试的结果代替本次证据。\
协作工具只能用 `agents.send_message` 向 `/root` 回报，不能联系其他子代理；需要其他子代理配合时，把依赖写进回报，由主代理协调。\
只在遇到阻塞、需要主代理决定，或发现会改变主代理计划或其他并行任务的事实时立即发消息，例如任务前提不成立、范围明显超出预期、其他写入者的改动导致你的检查失败；常规进度、中间发现和最终结果都写进最终回报，不要单独发送进度消息。";

pub(crate) fn subagent_source_config(role: &str) -> Option<&'static str> {
    match role {
        "codey_explorer" => Some(EXPLORER_AGENT_CONFIG),
        "codey_worker" => Some(WORKER_AGENT_CONFIG),
        "codey_reviewer" => Some(REVIEWER_AGENT_CONFIG),
        "default" => Some(DEFAULT_AGENT_CONFIG),
        _ => None,
    }
}

/// Codey owns `codex-constraints/` outright: every file there is regenerated
/// from these definitions on each launch.
pub(crate) const CODEY_FASTCTX_GUIDANCE: &str = "Codey FastCtx context tools are enabled as direct \
tools in native clients. Prefer FastCtx for supported local file operations. Use `mcp__codey_fastctx__inspect_local_file` for focused inspection, \
`mcp__codey_fastctx__grep` for search, `mcp__codey_fastctx__glob` for discovery, and \
`mcp__codey_fastctx__replace` only for deterministic replacement. Batch 2-32 known text files or ranges \
per inspect call; limit large files to needed ranges. A top-level `limit` applies to entries without one. \
Pass plain absolute filesystem paths; convert local URIs and Windows paths to a drive-letter path such as \
`E:/repo/file.ts`. Start broad grep with `files_with_matches`, then request minimal `content`; use summary \
or count only for totals. Glob with `filter_mode=ignore`, stable sorting, and `output_mode=details` only \
when metadata matters. Run replace as dry-run first with `max_replacements`, then inspect and test; never \
transparently retry a write after transport failure. Follow every Complete or Partial continuation without \
parallel page speculation. In native clients FastCtx is a direct-only tool namespace, not an MCP Resources \
server or code-mode aggregate; call tools directly and use `tool_search` when deferred and available. \
If a client explicitly provides a tool catalog through a transport-only endpoint, use that documented \
endpoint with the exact catalog name and argument schema, preserving the full result and continuation \
metadata. A catalog-listed FastCtx tool is available even without a same-named direct schema. Never \
invent wrappers or treat an execution aggregate as a transport-only endpoint. Use terminal commands for \
builds, tests, Git, package managers, advanced shell/streaming operations, unsupported metadata, or after \
the applicable FastCtx tool is unavailable or fails. Use CodeGraph only for semantic symbols and call \
paths. Every tool call must advance the task; put progress and corrections in commentary.";

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
    let marker = "\n\"\"\"\n";
    let replacement = format!("\n\n{guidance}\n\"\"\"\n");
    DEFAULT_AGENT_CONFIG.replacen(marker, &replacement, 1)
}

pub(crate) fn codey_fastctx_guidance_blocks(current: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let guidance = CODEY_FASTCTX_GUIDANCE;
    if current.contains(guidance) {
        blocks.push(guidance.to_string());
    }
    if let Some(prefix_end) = guidance.find(DEFAULT_FASTCTX_TOOL_NAMESPACE) {
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
    if guidance_paragraph_start(existing, ROOT_AGENT_COLLABORATION_USAGE_HINT).is_some() {
        return existing.to_string();
    }
    let mut updated = existing.trim_end().to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(ROOT_AGENT_COLLABORATION_USAGE_HINT);
    updated
}

pub(crate) fn remove_codey_fastctx_guidance(current: &str) -> Option<String> {
    let blocks = codey_fastctx_guidance_blocks(current);
    if blocks.is_empty() {
        return None;
    }
    let mut restored = current.to_string();
    let mut changed = false;
    for guidance in blocks {
        while let Some(without_guidance) = remove_owned_guidance_paragraph(&restored, &guidance) {
            restored = without_guidance;
            changed = true;
        }
    }
    changed.then_some(restored)
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
        assert!(config.ends_with("put progress and corrections in commentary.\n\"\"\"\n"));
    }

    #[test]
    fn tool_guidance_distinguishes_documented_transports_from_execution_aggregates() {
        for guidance in [ROOT_AGENT_COLLABORATION_USAGE_HINT, CODEY_FASTCTX_GUIDANCE] {
            assert!(guidance.contains("transport-only endpoint"));
            assert!(guidance.contains("exact catalog name and argument schema"));
            assert!(guidance.contains("without a same-named direct schema"));
        }
        assert!(
            CODEY_FASTCTX_GUIDANCE.contains("preserving the full result and continuation metadata")
        );
        assert!(CODEY_FASTCTX_GUIDANCE.contains("Never invent wrappers"));
        assert!(
            CODEY_FASTCTX_GUIDANCE.contains("Prefer FastCtx for supported local file operations")
        );
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
        assert!(combined.contains("after counting dispatch"));
        assert!(combined.contains("direct commentary tools"));
        assert!(combined.contains("`timeout_ms: 120000`"));
        assert!(combined.contains("mailbox updates are not completion"));
        assert!(combined.contains("`MESSAGE`"));
        assert!(combined.contains("spawn just to fill slots"));
        assert!(combined.contains("Give one `codey_explorer` all related"));
        assert!(combined.contains("Roles share `workspace-write` and tool capabilities"));
        assert!(combined.contains("role instructions guide `codey_explorer` to stay read-only"));
        for retired in [
            "claim their complete target paths",
            "claim workspace writing",
            "claim writing resources",
            "Claims last until the attempt settles",
            "Multi-file claims are all-or-nothing",
            "Task prose cannot narrow actual tool scope",
        ] {
            assert!(!combined.contains(retired), "{retired}");
        }
        assert!(combined.contains("re-read a file before editing it"));
        assert!(combined.contains("isolated worktree or copy"));
        assert!(combined.contains("serialize them or use an"));
        assert!(!combined.contains("`CODEY_SUBAGENT_WORKSPACE_BUSY`"));
        assert!(combined.contains("`agents.send_message`"));
        assert!(combined.contains("prefer it over a fresh spawn"));
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
        assert!(combined.contains("even from a later turn"));
        assert!(!combined.contains("active-looking provider state stale"));
        assert!(combined.contains("terminal or fenced"));
        assert!(combined.contains("unfiltered `agents.list_agents`"));
        assert!(combined.contains("if a slot is free"));
        assert!(combined.contains("planned, unspawned task"));
        assert!(combined.contains("all planned work has been performed"));
        assert!(combined.contains("do not loop on an unregistered tool"));
        assert!(combined.contains("`functions.exec` as a JavaScript aggregate"));
        assert!(combined.contains("transport-only endpoint"));
        assert!(combined.contains("exact catalog name and argument schema"));
        assert!(combined.contains("without a same-named direct schema"));
        assert!(
            combined.contains("Do not invent wrappers or bypass lifecycle and permission checks")
        );
        assert!(!combined.contains("Write-Output"));
        assert!(!combined.contains("Write-Error"));
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&combined),
            combined
        );
        let current_before_user = format!(
            "{}\n\nPreserve this position.",
            root_agent_collaboration_usage_hint()
        );
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&current_before_user),
            current_before_user
        );
    }

    #[test]
    fn multi_agent_mode_hint_uses_uniform_concurrency_without_spawn_budgets() {
        for role in crate::config::SUBAGENT_ROLE_IDS
            .into_iter()
            .filter(|role| *role != crate::config::SUBAGENT_ROLE_DEFAULT)
        {
            assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains(role), "{role}");
        }
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("Always set `agent_type`"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("At most two sub-agents run at once"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("Sub-agents never spawn"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("`CODEY_SUBAGENT_CONCURRENCY_LIMIT`"));
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("cannot decrypt its task body"));
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("`agents.send_message` exactly once"));
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("do not interrupt or respawn it"));
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("`CODEY_SUBAGENT_DUPLICATE_TASK_ID`"));
        assert!(
            !ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("CODEY_SUBAGENT_BATCH_BUDGET_EXHAUSTED")
        );
        assert!(!ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("CODEY_SUBAGENT_TURN_BUDGET_EXHAUSTED"));
    }

    #[test]
    fn multi_agent_mode_hint_fits_codex_byte_budget() {
        let bytes = ROOT_AGENT_MULTI_AGENT_MODE_HINT.as_bytes();
        assert!(
            ROOT_AGENT_MULTI_AGENT_MODE_HINT.len() <= 1600,
            "{}",
            ROOT_AGENT_MULTI_AGENT_MODE_HINT.len()
        );
        let head = std::str::from_utf8(&bytes[..bytes.len().min(800)]).unwrap_or_else(|error| {
            panic!(
                "mode hint first 800 bytes split UTF-8: {error}; prefix bytes: {:?}",
                String::from_utf8_lossy(
                    &bytes[error.valid_up_to().saturating_sub(24)
                        ..bytes.len().min(error.valid_up_to() + 24)]
                )
            )
        });
        assert!(head.contains("The root handles clear bounded work directly"));
    }

    #[test]
    fn root_does_not_poll_children_for_progress() {
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("Do not request interim progress"));
        assert!(SUBAGENT_TASK_BOUNDARY_GUARD.contains("不要单独发送进度消息"));
    }

    #[test]
    fn mode_hint_defaults_to_root_work_and_names_what_stays_there() {
        let hint = ROOT_AGENT_MULTI_AGENT_MODE_HINT;
        assert!(hint.contains("The root handles clear bounded work directly"));
        assert!(hint.contains("including a few searches, a known edit"));
        assert!(hint.contains("startup, task description, waiting, validation and merge costs"));
        assert!(hint.contains("clearly larger than a handful of tool calls"));
        assert!(hint.contains("Quick, medium and thorough state scope and stopping conditions"));
        assert!(hint.contains("none implies a cheaper model is better"));
        assert!(hint.contains("Keep related questions in one explorer context"));
        assert!(hint.contains("split only genuinely unrelated work"));
        assert!(hint.contains("Do not delegate to fill slots"));
        assert!(hint.contains("classification or summary agents"));
        for kept in [
            "multi-step read-only research",
            "implementation",
            "completed stable input",
            "raw output it must read",
        ] {
            assert!(hint.contains(kept), "{kept}");
        }
        assert!(!hint.contains("Proactive multi-agent delegation is active"));
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("spawn just to fill slots"));
        assert!(!ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("dispatch independent tasks up to"));
        assert!(
            ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("Never put passwords, tokens or keys")
        );
    }

    #[test]
    fn active_guidance_drops_retired_write_claim_semantics() {
        for text in [
            ROOT_AGENT_COLLABORATION_USAGE_HINT,
            ROOT_AGENT_MULTI_AGENT_MODE_HINT,
        ] {
            for retired in [
                "CODEY_SUBAGENT_WORKSPACE_BUSY",
                "CODEY_SUBAGENT_ADMISSION_BUSY",
                "CODEY_SUBAGENT_WRITE_SCOPE_LIMIT",
                "写入占用",
                "写入预留",
                "claim writing resources",
                "writer reservation",
                "all-or-nothing",
            ] {
                assert!(!text.contains(retired), "{retired}");
            }
        }
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("Assign distinct files"));
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("Git may expose merge conflicts"));
    }

    #[test]
    fn worker_role_runs_its_own_verification_commands() {
        assert!(!WORKER_AGENT_CONFIG.contains("契约显式授予"));
        assert!(WORKER_AGENT_CONFIG.contains("命令被拒绝或无法运行时"));
        assert!(WORKER_AGENT_CONFIG.contains("不要格式化或批量改写本次未修改的文件"));
        assert!(WORKER_AGENT_CONFIG.contains("其他写入者正在修改的代码时不要去修"));
        assert!(
            ROOT_AGENT_COLLABORATION_USAGE_HINT
                .contains("Workers run checks for their changed scope")
        );
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("integration or uncovered checks"));
    }

    #[test]
    fn role_templates_split_by_function_not_depth_or_domain() {
        assert!(
            DEFAULT_AGENT_CONFIG
                .contains("仅在任务需要用户图片、截图、页面、GUI 或渲染证据时使用视觉工具")
        );
        assert!(EXPLORER_AGENT_CONFIG.contains("你是检索子代理，只读"));
        assert!(EXPLORER_AGENT_CONFIG.contains("未写明时按中等"));
        assert!(EXPLORER_AGENT_CONFIG.contains("优先使用读取、搜索和查看现有图片的工具"));
        assert!(
            EXPLORER_AGENT_CONFIG.contains("也可执行 `git status/diff/log`、日志查询等只读命令")
        );
        assert!(!EXPLORER_AGENT_CONFIG.contains("只有当前轮原生只读沙箱已确认时"));
        assert!(!EXPLORER_AGENT_CONFIG.contains("通用执行包装、界面操作和未归类工具不可用"));
        assert!(WORKER_AGENT_CONFIG.contains("前端界面与交互、后端"));
        assert!(
            WORKER_AGENT_CONFIG
                .contains("改动影响用户可见界面时，优先用客户端已授权的渲染或截图工具核验实际呈现")
        );
        assert!(REVIEWER_AGENT_CONFIG.contains("不修改源码、不修复问题"));
        assert!(REVIEWER_AGENT_CONFIG.contains("不要仅凭代码或文件名猜测视觉结果"));
        for template in [
            DEFAULT_AGENT_CONFIG,
            EXPLORER_AGENT_CONFIG,
            WORKER_AGENT_CONFIG,
            REVIEWER_AGENT_CONFIG,
        ] {
            assert!(!template.contains(r"\n"));
            assert!(template.contains("status: completed | partial | blocked"));
        }
        for role in crate::config::SUBAGENT_ROLE_IDS {
            assert!(subagent_source_config(role).is_some(), "{role}");
        }
    }

    #[test]
    fn root_hint_lists_the_exact_active_gate_read_allowlist() {
        let expected = "`read_file`, \
`inspect_local_file`, `grep`, `glob`, and `view_image`";
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains(expected));
        assert!(
            ROOT_AGENT_COLLABORATION_USAGE_HINT
                .contains("While only `codey_explorer` children run")
        );
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("keep working on independent files"));
        assert!(
            ROOT_AGENT_COLLABORATION_USAGE_HINT
                .contains("Any other active child limits you to `agents.*` tools")
        );
        assert!(!ROOT_AGENT_COLLABORATION_USAGE_HINT.contains(r"\n"));
    }

    #[test]
    fn children_are_told_to_report_only_to_root() {
        assert!(SUBAGENT_TASK_BOUNDARY_GUARD.contains("向 `/root` 回报，不能联系其他子代理"));
        assert!(SUBAGENT_TASK_BOUNDARY_GUARD.contains("不要单独发送进度消息"));
        assert!(SUBAGENT_TASK_BOUNDARY_GUARD.contains("收到任务或追派都无需为此发消息"));
        assert!(SUBAGENT_TASK_BOUNDARY_GUARD.contains("其他写入者的改动导致你的检查失败"));
        assert!(SUBAGENT_TASK_BOUNDARY_GUARD.contains("完成后收到追派时沿用已有上下文"));
    }

    #[test]
    fn role_sandboxes_match_their_actual_capabilities() {
        for role in crate::config::SUBAGENT_ROLE_IDS {
            let source = subagent_source_config(role).unwrap();
            assert!(source.contains(&format!("name = \"{role}\"")));
            assert!(source.contains("description = \""));
        }
        assert!(EXPLORER_AGENT_CONFIG.contains("sandbox_mode = \"workspace-write\""));
        assert!(REVIEWER_AGENT_CONFIG.contains("sandbox_mode = \"workspace-write\""));
        assert!(WORKER_AGENT_CONFIG.contains("sandbox_mode = \"workspace-write\""));
        assert!(DEFAULT_AGENT_CONFIG.contains("sandbox_mode = \"workspace-write\""));
    }

    #[test]
    fn fastctx_guidance_cleanup_removes_current_codey_owned_guidance() {
        let user_server_guidance =
            CODEY_FASTCTX_GUIDANCE.replace(DEFAULT_FASTCTX_TOOL_NAMESPACE, "mcp__fastctx");
        let configured = format!(
            "User guidance.\n\n{CODEY_FASTCTX_GUIDANCE}\n\n{user_server_guidance}\n\nConcurrent guidance.",
        );

        assert_eq!(
            remove_codey_fastctx_guidance(&configured).as_deref(),
            Some("User guidance.\n\nConcurrent guidance.")
        );
    }

    #[test]
    fn fastctx_guidance_blocks_detect_user_fastctx_namespaces() {
        let user_server_guidance =
            CODEY_FASTCTX_GUIDANCE.replace(DEFAULT_FASTCTX_TOOL_NAMESPACE, "mcp__context_tools");
        let configured = format!("Prefix\n\n{user_server_guidance}\n\nSuffix");

        assert_eq!(
            codey_fastctx_guidance_blocks(&configured),
            vec![user_server_guidance]
        );
    }
}
