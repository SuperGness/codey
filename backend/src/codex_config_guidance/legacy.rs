pub(super) const DELEGATION_V2_GUIDANCE: &str = concat!(
    "## 子代理使用\n",
    "\n",
    "子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。",
    "\n",
    "\n",
    "### 自适应委派\n",
    "\n",
    "以下任务直接处理，不派子代理：\n",
    "\n",
    "- 回答、解释、状态汇报、单一事实或一次精确查询；\n",
    "- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；\n",
    "- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；\n",
    "- 派发、等待和抽查成本不明显低于直接处理的任务。\n",
    "\n",
    "任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 ",
    "个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` ",
    "表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` ",
    "表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。\n",
    "\n",
    "这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。默认最多派生",
    " 2 个子代理；超过 2 个还必须有 3 个以上真正独立的分支，或用户明确要求大规模并行。不要用 verifier 重复已有确定性测试。\n",
    "\n",
    "每轮采用可恢复成本点预算：快速定位和 `default` 为 1 点，深度/视觉分析为 2 点，写入角色为 3 点；普通上限 8 点。只有 `why: \"parallel\"` 且提供真实的 ",
    "`branch_calls` 时，才按独立分支数自适应提高预算，最高 12 点；`user_requested` ",
    "只表达用户确实要求委派，不会自行放大预算。该账本衡量派生尝试、角色成本和时限，不冒充上游未提供的精确 token 或费用。\n",
    "\n",
    "### 任务类型与运行时契约\n",
    "\n",
    "派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 ",
    "`codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ",
    "ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。\n",
    "\n",
    "每个 `agents.spawn_agent` 必须使用 `fork_turns=\"none\"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 ",
    "message 的最后一个非空行：\n",
    "\n",
    "`CODEY_DELEGATION_V2={\"id\":\"task_name\",\"why\":\"breadth\",\"visual\":false,\"read\":[],\"write\":[],\"checks\":[]}`",
    "\n",
    "\n",
    "`id` 必须等于 `task_name`。只有 `why: \"parallel\"` 时才填写 `branch_calls`，用数组记录各独立分支预计的实质调用数，例如 ",
    "`\"branch_calls\":[3,3]`；其他路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` ",
    "后使用相对路径。只声明本任务确实会使用的稳定路径；读取可以留空，写入角色的 `write` 不得为空。写入角色还必须提供 1–3 个精确检查，例如 ",
    "`\"checks\":[{\"id\":\"tests\",\"cmd\":\"cargo test -p app\"}]`。运行时会校验理由与角色兼容性、角色能力、预算以及 read/write ",
    "父子路径冲突；重复任务 ID、越权写入和重叠 ownership 仍会被拒绝。\n",
    "\n",
    "### 任务胶囊与验收\n",
    "\n",
    "- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。\n",
    "- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 ",
    "`file:line`、符号名或可复核链接。\n",
    "- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。\n",
    "- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` ",
    "标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 ",
    "分钟无法验证时，门禁会把该项标记为\u{201c}未验证\u{201d}并在一次明确提示后释放；主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。\n",
    "- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。\n",
    "\n",
    "### 汇合、异常与接管\n",
    "\n",
    "- 同一批独立任务先全部派发，再进入等待；有活动子代理时只使用 `agents.*` 协调，直至每个代理进入终态。\n",
    "- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。\n",
    "- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 ",
    "`agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` ",
    "最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。\n",
    "- `MESSAGE` ",
    "或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 ",
    "`task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。\n",
    "- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态则只中断一次并继续等待。禁止无限 ",
    "wait、无限重试或静默重派。\n",
    "- 用户新输入使旧批次失效时，先中断活动代理并完成对账，忽略迟到结果。预算账本和未清偿验收债按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。\n",
);

pub(super) const BATCH_RESOLUTION_USAGE_HINT: &str = concat!(
    "`agents.spawn_agent`, `agents.wait_agent`, and every other `agents` collaboration tool are ",
    "direct commentary tools. Call them only through their declared direct tool schemas. Dispatch ",
    "every independent agent planned for the current batch before the first wait, then call ",
    "`agents.wait_agent` before any non-collaboration work. Use `timeout_ms <= 120000`; a mailbox ",
    "update is not completion. If a `MESSAGE` or another partial update needs action, use only the ",
    "relevant `agents.send_message`, `agents.followup_task`, `agents.interrupt_agent`, or ",
    "`agents.list_agents` tool, then return to `agents.wait_agent`. Use `followup_task` only while ",
    "the target's current attempt is still nonterminal and bound; never use it to reactivate a ",
    "terminal, failed, shutdown, not_found, or prior-turn agent. A ",
    "`CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` denial happens before the child is woken: do ",
    "not retry or wait for that canonical task to recover; use a fresh `task_name` with the same ",
    "fresh `CODEY_DELEGATION_V2.id` through `agents.spawn_agent`, or let the root take over. Treat ",
    "`FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and ",
    "`not_found` as terminal; `pending_init`, `running`, and `interrupted` remain nonterminal. If a ",
    "wait result lacks per-agent terminal details, call unfiltered `agents.list_agents` to reconcile ",
    "the full batch. Continue until every spawned agent is terminal. Once the batch settles, call ",
    "`mcp__codey_subagent_control__resolve_batch` with the reported batch number, a unique ",
    "decision_id, and a short reason. Choose exactly one of `spawn_next_batch`, `continue_root`, ",
    "`complete`, or `blocked`; `spawn_next_batch` must be followed by direct `agents.spawn_agent` ",
    "calls in the same response, while `continue_root` must later be replaced by `complete`, ",
    "`blocked`, or `spawn_next_batch` before Stop. If a collaboration tool is unavailable, do not ",
    "loop on an unregistered tool; Codey's runtime gate has bounded stale-state recovery. While ",
    "spawned subagents are active, Codey's runtime gate denies non-collaboration local tools and ",
    "prevents the root turn from finishing. The `functions.exec` tool world is a separate route and ",
    "does not contain collaboration tools.",
);

pub(super) const DIRECT_SCHEMA_USAGE_HINT: &str = concat!(
    "`agents.spawn_agent`, `agents.wait_agent`, and every other `agents` collaboration tool are ",
    "direct commentary tools. Call them only through their declared direct tool schemas. Dispatch ",
    "every independent agent planned for the current batch before the first wait, then call ",
    "`agents.wait_agent` before any non-collaboration work. Use `timeout_ms <= 120000`; a mailbox ",
    "update is not completion. If a `MESSAGE` or another partial update needs action, use only the ",
    "relevant `agents.send_message`, `agents.followup_task`, `agents.interrupt_agent`, or ",
    "`agents.list_agents` tool, then return to `agents.wait_agent`. Treat `FINAL_ANSWER`, ",
    "`task_complete`, `completed`, `errored`, `shutdown`, and `not_found` as terminal; ",
    "`pending_init`, `running`, and `interrupted` remain nonterminal. If a wait result lacks ",
    "per-agent terminal details, call unfiltered `agents.list_agents` to reconcile the full batch. If",
    " a collaboration tool is unavailable, do not loop on an unregistered tool; Codey's runtime gate ",
    "has bounded stale-state recovery. Continue until every spawned agent is terminal. While spawned ",
    "subagents are active, Codey's runtime gate denies non-collaboration local tools and prevents the",
    " root turn from finishing. The `functions.exec` tool world is a separate route and does not ",
    "contain collaboration tools.",
);

pub(super) const TASK_ROUTED_FASTCTX_GUIDANCE: &str = concat!(
    "Codey FastCtx context tools are enabled as direct tools. Route local workspace work by task: use",
    " `mcp__codey_fastctx__inspect_local_file` for focused inspection, `mcp__codey_fastctx__grep` for",
    " content search, `mcp__codey_fastctx__glob` for path discovery, and ",
    "`mcp__codey_fastctx__replace` only for deterministic mechanical replacement. Batch 2-32 ",
    "already-known text files or ranges into one `inspect_local_file(files=[...])` call; for large ",
    "files request only the needed ranges. Pass plain absolute filesystem paths, converting ",
    "URI-shaped local references and Windows paths to a drive-letter path such as `E:/repo/file.ts`. ",
    "Start broad searches with grep's `files_with_matches`, then use `content` with the smallest ",
    "useful context and result limit; use `summary` or `count` only when totals are the requested ",
    "result. Use glob with `filter_mode=project` and stable path sorting unless ignored files or ",
    "modification order are explicitly required. Run replace as dry-run first, set ",
    "`max_replacements`, then inspect or grep the result and run relevant tests; never transparently ",
    "retry a write after transport failure. Follow every Complete or Partial continuation exactly and",
    " do not speculatively request later pages in parallel. FastCtx is a direct-only tool namespace, ",
    "not an MCP Resources server and not a code-mode aggregate; call the tools directly, using ",
    "`tool_search` when a direct tool is deferred. Use terminal commands for builds, tests, Git, ",
    "package managers, metadata or streaming operations, advanced shell semantics, or after the ",
    "applicable FastCtx tool is unavailable or fails. Use CodeGraph only for semantic code ",
    "understanding such as symbols, callers, callees, and call paths. Every tool call must advance ",
    "the requested task; put progress and corrections in commentary.",
);

pub(super) const DIRECT_SUBAGENT_GUIDANCE: &str = concat!(
    "## 子代理使用\n",
    "\n",
    "主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。",
    "\n",
    "\n",
    "纯只读工作最多同时运行 3 个子代理；存在写入型或身份未确认的代理时最多同时运行 2 个。并发限制只约束同时运行数量，不限制后续派发次数。\n",
    "\n",
    "### 派发\n",
    "\n",
    "- 直接调用 `agents.spawn_agent`，按任务选择 ",
    "`codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 ",
    "`codey_visual_worker`；`default` 仅兼容旧配置。`task_name` 只含小写字母、数字和下划线。\n",
    "- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景，不复制整段对话，不附加 V1/V2 契约、sidecar、checks 或其他尾行协议。\n",
    "- 修改关键代码或文档时，可先派发独立的只读调查或核验；写入任务明确文件归属，避免重复调查或同时修改同一处。只读角色获得 `files.read`；写入角色获得 ",
    "`command.execute`、`files.read` 和 `workspace.write`。写入角色暂按当前工作区建立互斥锁；实际权限仍由 Codex 原生 ",
    "sandbox、approval policy、permission profile 和 writable roots 决定。\n",
    "\n",
    "### 返回与验收\n",
    "\n",
    "- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 ",
    "`file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。\n",
    "- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收；Codey 不再创建逐任务机械验收债或强制验收命令。\n",
    "\n",
    "### 生命周期\n",
    "\n",
    "- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，按下一个计划任务的角色重新计算并发上限；存在空余槽位时立即使用新 ",
    "`task_name` 补位，否则继续等待。所有计划任务均已派发后，继续等待剩余活动 attempt 结算。活动 attempt 期间只使用必要的 `agents.*` ",
    "协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。\n",
    "- `MESSAGE` ",
    "只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 ",
    "`task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。\n",
    "- 成功的 `agents.interrupt_agent` 会永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 ",
    "`agents.list_agents` 对账：原代理存在则等待或消费结果，不存在则由根代理接管。只有任务范围实质改变时才用全新 task ID 最多重派一次。\n",
    "- 协作工具不可用时不要循环调用；依赖有界的 pending-init、超时和 Stop 恢复路径收敛。\n",
);

pub(super) const DIRECT_COLLABORATION_USAGE_HINT: &str = concat!(
    "`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools are direct ",
    "commentary tools; never call them through `functions.exec`. Dispatch up to the current ",
    "concurrency limit from the planned independent work before the first wait. While any attempt is ",
    "active, use only the relevant `agents.spawn_agent`, `agents.send_message`, ",
    "`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. ",
    "After a terminal or successfully fenced update, recompute the role-aware concurrency limit; if ",
    "it exposes a slot, immediately use `agents.spawn_agent` with a new `task_name` for the next ",
    "planned, unspawned task; otherwise return to `agents.wait_agent` with `timeout_ms: 30000`. ",
    "`MESSAGE` and mailbox updates are not completion. Use `followup_task` only for a bound ",
    "nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not ",
    "retry or wait for that target; take over or use a fresh `task_name` for a materially changed ",
    "task. Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, ",
    "`shutdown`, and `not_found` as terminal. A successful root interrupt permanently abandons and ",
    "fences that attempt and settles it for the lifecycle ledger; do not wait for or follow up that ",
    "target. Interrupt revokes child tool access before the provider call, but keeps the writer ",
    "reservation until acknowledgement or terminal reconciliation. Queued followups may still start ",
    "another turn; they cannot restore tool access. A failed interrupt does not release the ",
    "reservation. Match evidence and write reports to the exact task and attempt; never transfer ",
    "another attempt's no-change claim or infer a transport stall from elapsed time alone. If a wait ",
    "times out or lacks per-agent terminal details, call unfiltered `agents.list_agents` before ",
    "waiting again. Continue until all planned work has been spawned and every attempt is terminal or",
    " fenced. Then the root agent validates the combined result and either continues the work or ",
    "finishes. While an attempt is active, Codey's gate blocks non-collaboration tools and Stop. If ",
    "collaboration tools are unavailable, do not loop on an unregistered tool.",
);

pub(super) const DIRECT_FASTCTX_GUIDANCE: &str = concat!(
    "Codey FastCtx context tools are enabled as direct tools. Use ",
    "`mcp__codey_fastctx__inspect_local_file` for focused inspection, `mcp__codey_fastctx__grep` for ",
    "search, `mcp__codey_fastctx__glob` for discovery, and `mcp__codey_fastctx__replace` only for ",
    "deterministic replacement. Batch 2-32 known text files or ranges per inspect call; limit large ",
    "files to needed ranges. A top-level `limit` applies to entries without one. Pass plain absolute ",
    "filesystem paths; convert local URIs and Windows paths to a drive-letter path such as ",
    "`E:/repo/file.ts`. Start broad grep with `files_with_matches`, then request minimal `content`; ",
    "use summary or count only for totals. Glob with `filter_mode=ignore`, stable sorting, and ",
    "`output_mode=details` only when metadata matters. Run replace as dry-run first with ",
    "`max_replacements`, then inspect and test; never transparently retry a write after transport ",
    "failure. Follow every Complete or Partial continuation without parallel page speculation. ",
    "FastCtx is a direct-only tool namespace, not an MCP Resources server or code-mode aggregate; ",
    "call tools directly and use `tool_search` when deferred. Use terminal commands for builds, ",
    "tests, Git, package managers, advanced shell/streaming operations, unsupported metadata, or ",
    "after the applicable FastCtx tool is unavailable or fails. Use CodeGraph only for semantic ",
    "symbols and call paths. Every tool call must advance the task; put progress and corrections in ",
    "commentary.",
);

/// Historical default text (sha1 7ef421f5) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_8_2: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。已确认的纯只读批次最多同时运行 3 个子代理；批次包含写入型或身份未确认的代理时，最多同时运行 2 个。不要用 verifier 重复已有确定性测试。

Codey 不再按角色成本点、每批尝试次数、批次数或根回合累计尝试数设置派发预算上限。编排账本只记录生命周期、批次决策、验收债和恢复所需元数据；并发限制只约束同时运行数量，不限制后续批次或累计派发次数，长期 Goal 任务也可以在同一根回合继续多个批次。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`；路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径。只声明本任务确实会读取和写入的稳定路径；省略 `root` 时采用 Hook 工作目录，空 `read` 对只读角色表示该 root、对写入角色表示其 `write` 范围。V2 契约必须显式声明 `files.read`；写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。worker 只有额外声明 `command.execute` 且 `write` 覆盖完整 root 时才能运行通用 shell。子代理网络能力不开放，契约不能自行声明或放宽。路径读取和写入都会按解析现存祖先后的物理路径校验；未绑定 attempt 不获得数据或副作用权限。运行时还会校验工具的完整可信名称、理由、角色能力、角色感知并发上限及 read/write 冲突；重复任务 ID、越权访问和重叠 ownership 会被拒绝。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；同一根 turn 可继续完成本批派发与必要协调，child turn 或缺失/不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次并继续等待，否则依赖 Stop 的受控恢复路径 fence 遗留 attempt。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先完成对账；只有可信根 turn 绑定仍有效时才中断活动代理，否则让受控恢复路径接管并忽略迟到结果。编排账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 bbf122bb) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_8_3: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。已确认的纯只读批次最多同时运行 3 个子代理；批次包含写入型或身份未确认的代理时，最多同时运行 2 个。不要用 verifier 重复已有确定性测试。

Codey 不再按角色成本点、每批尝试次数、批次数或根回合累计尝试数设置派发预算上限。编排账本只记录生命周期、批次决策、验收债和恢复所需元数据；并发限制只约束同时运行数量，不限制后续批次或累计派发次数，长期 Goal 任务也可以在同一根回合继续多个批次。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`；路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径。只声明本任务确实会读取和写入的稳定路径；省略 `root` 时采用 Hook 工作目录，空 `read` 对只读角色表示该 root、对写入角色表示其 `write` 范围。V2 契约必须显式声明 `files.read`；写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。worker 只有额外声明 `command.execute` 且 `write` 覆盖完整 root 时才能运行通用 shell。子代理网络能力不开放，契约不能自行声明或放宽。路径读取和写入都会按解析现存祖先后的物理路径校验；未绑定 attempt 不获得数据或副作用权限。运行时还会校验工具的完整可信名称、理由、角色能力、角色感知并发上限及 read/write 冲突；重复任务 ID、越权访问和重叠 ownership 会被拒绝。

`root` 必须是子代理实际执行任务的工作树根。目标位于 `.worktrees/<name>` 或其他隔离 worktree 时，必须把该 worktree 的绝对路径写入 `root`，并让相对 `read`/`write` 以它为基准；不得用主 checkout 的 root 代替。路径工具的相对目标会按 child `cwd` 解析后再与契约比较；缺少可信 `cwd` 时相对访问按 fail-closed 拒绝。读取被门禁拒绝后，子代理应立即向根代理回报，由根代理使用新任务 ID 和正确契约重新派发或直接接管；不得改用 Bash 绕过，验证命令仍由根代理执行。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；同一根 turn 可继续完成本批派发与必要协调，child turn 或缺失/不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次并继续等待，否则依赖 Stop 的受控恢复路径 fence 遗留 attempt。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先完成对账；只有可信根 turn 绑定仍有效时才中断活动代理，否则让受控恢复路径接管并忽略迟到结果。编排账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 5821f2b4) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_8_7: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。已确认的纯只读批次最多同时运行 3 个子代理；批次包含写入型或身份未确认的代理时，最多同时运行 2 个。不要用 verifier 重复已有确定性测试。

Codey 不再按角色成本点、每批尝试次数、批次数或根回合累计尝试数设置派发预算上限。编排账本只记录生命周期、批次决策、验收债和恢复所需元数据；并发限制只约束同时运行数量，不限制后续批次或累计派发次数，长期 Goal 任务也可以在同一根回合继续多个批次。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`；路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径；绝对 claim 可以位于 `root` 之外，跨多个工作树时分别声明即可。`root`、`read` 和 `write` 是冲突协调、ownership 与审计元数据，应尽量准确描述预计范围，但不是 Codey 的运行时文件路径 ACL。省略 `root` 时采用有效的 Hook 工作目录；Hook 目录缺失或无效但 claim 已是绝对路径时仍可派发。空 `read` 会对只读角色以 root、对写入角色以其 `write` 作为默认协调 claim，但不会收窄 Codex 原生可访问范围。V2 契约必须显式声明 `files.read`；需要 Git 状态、差异或其他终端读取的任务额外声明 `command.execute`。`command.execute` 只表示允许调用命令工具，不代表获得文件写权限：只读角色可在 `write=[]`、`checks=[]` 时声明它，写入仍由角色配置及继承的 Codex sandbox、approval policy 和 permission profile 决定。写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`；通用 shell 不要求 `write` 覆盖完整 root。上游把任务正文替换为不可读密文时，只读兼容 reservation 会保留 `files.read` 与 `command.execute`，但仍不能形成写 ownership 或验收债。受信任的 Network 类网页搜索、打开和截图属于只读检索，活动 attempt 声明 `files.read` 即可使用，不要求额外网络 capability，也不会因此获得本地写入；实际联网仍受 Codex 原生网络与审批设置约束。未绑定 attempt 不获得数据或副作用权限；只读角色即使出现异常或旧账本也不能取得直接写入工具能力。运行时还会校验工具的完整可信名称、理由、角色能力、角色感知并发上限及 read/write 冲突；重复任务 ID 和重叠 ownership 会被拒绝。

Codey 在 attempt 身份和角色 capability 校验通过后，不再解析、比较或拒绝读取、写入和命令中的目标路径，也不要求 child `cwd` 与契约 `root` 相同。子代理实际能访问哪些工作树、兄弟目录或外部路径，完全由其继承的 Codex sandbox、approval policy、permission profile 和 writable roots 决定；原生权限拒绝时按 Codex 的正常流程处理，不应通过修改 Codey 契约冒充权限提升。验证命令仍由根代理执行机械验收。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；同一根 turn 可继续完成本批派发与必要协调，child turn 或缺失/不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次。成功的 `agents.interrupt_agent` 表示根代理永久放弃该 attempt，Codey 会立即 fence 并核销本地活动状态；不要再等待或追派该 target，直接接管。中断失败或目标无法唯一匹配时才继续对账，并依赖 Stop 的受控恢复路径。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先完成对账；只有可信根 turn 绑定仍有效时才中断活动代理。中断成功后立即接管并忽略迟到结果，不再等待上游把 `interrupted` 另行改成终态；否则让受控恢复路径接管。编排账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 0dfcfd3d) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_9_7: &str = r#"## 子代理使用

默认由主代理直接处理短而明确、步骤互相依赖或即将修改关键代码/文档的任务；不要为了形式分工而派生。以下情况才积极考虑子代理：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次，`breadth` 表示至少 2 个目录或 4 个候选文件，`context` 表示大量日志或文档会污染主线程，`independent_work` 表示不在主代理关键路径上的独立实现，`high_risk` 表示高风险结论缺少确定性检查，`user_requested` 仅在用户明确要求子代理或大规模并行时使用。不超过 2 个小文件和 3 次本地工具调用的精确任务通常由主代理完成。这些数量只是软路由参考。

纯只读批次最多同时运行 3 个子代理；存在写入型或身份未确认的代理时最多同时运行 2 个。Codey 不再按角色成本点设置预算上限，也不限制后续批次或累计派发次数，但并发、生命周期和验收门禁始终有效。

### 派发契约

- 按任务选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 仅兼容旧配置，主代理不得主动选择。实际权限仍受父任务约束。
- 每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 只含小写字母、数字和下划线，并把下面的单行 JSON 作为消息最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

- `id` 必须等于 `task_name`；路由规模只在派发判断中说明。`root`、`read`、`write` 是 ownership/冲突协调元数据，不是 Codey 的运行时文件路径 ACL；绝对路径可位于 `root` 外。实际文件与网络权限完全由其继承的 Codex sandbox、approval policy、permission profile 和 writable roots 决定，Codey 不再解析、比较或拒绝读取、写入和命令中的目标路径。
- 所有角色声明 `files.read`。Git 状态、差异或其他终端读取额外声明 `command.execute`；`command.execute` 只表示允许调用命令工具，只读角色可在 `write=[]`、`checks=[]` 时声明它；只读兼容 reservation 会保留 `files.read` 与 `command.execute`。Network 类网页搜索、打开和截图属于只读检索，声明 `files.read` 即可使用，不要求额外网络 capability。
- 写入角色还必须声明 `workspace.write`、非空 `write` 和 1–3 个精确 `checks`，不要求 `write` 覆盖完整 root。派发写入角色前先调用 `mcp__codey_subagent_control__prepare_delegation`，使用相同 task、角色、V2 契约和唯一 preparation ID；accepted 后立即 spawn。密文任务缺少匹配 sidecar 时交回主代理。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行必须是 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 写入结果由主代理按 Stop 给出的 `# codey-accept:<task>:<check>` 原样重跑。只有可信的退出状态 `0` 才清偿验收债；连续 3 次失败、连续 3 次无新证据或持续 10 分钟仍无法验证时，该批次只能以 `blocked` 结算并如实报告。

### 批次生命周期

- 同一批独立任务先全部派发，再进入等待。Hook 用本批首个根派生调用的可信 `turn_id` 绑定编排主体；child 或不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账。活动代理全部结算前不得恢复普通本地工作。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍按非终态处理，除非根代理成功中断并永久放弃该 attempt。
- 成功的 `agents.interrupt_agent` 表示根代理永久放弃并 fence 该 attempt；不要再等待或追派该 target，直接接管。`pending_init` 或运行 10 分钟仍无终态时先 list 对账，仍卡住且根绑定有效时只中断一次。
- 重复 task ID 时只 list 对账一次：原代理存在则等待/消费结果，不存在则主代理接管。失败后默认接管；只有缩小或改变范围后仍值得独立委派，才用全新 task ID 最多重派一次。follow-up 被拒绝表示旧 attempt 未唤醒，不得循环重试。
- 用户新输入使旧批次失效时先对账；仅在可信根绑定有效时中断活动代理。协作工具不可用时不循环调用不存在的工具。
"#;

/// Historical default text (sha1 321e8325) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_7_1: &str = r#"## 子代理使用

子代理用于把宽而重的检索、独立核验或边界清晰的实现从主线程中拆出。只有当委派能减少上下文污染、提高并行度或提供独立证据时才派发；已知位置的小文件、即将修改的确切代码和奠基性文档仍由主代理亲自读取。

### 任务类型路由

派生时必须按任务性质显式选择下列 `agent_type`，不要用模型名代替任务类型：

- `codey_quick_scan`：只读的快速定位、精确事实查找、重复性检查和低风险小范围检索。
- `codey_deep_research`：只读的跨文件、日志、代码或文档宽范围检索、归纳和架构探索。
- `codey_visual_analysis`：只读的截图、页面、GUI、PDF 等视觉证据分析，以及复杂探索或独立核验。
- `codey_worker`：可写的低到中等复杂度、边界清晰、可回滚且可测试的非视觉实现。
- `codey_visual_worker`：可写的页面、GUI、PDF 或其他依赖视觉证据与渲染验证的实现。
- `default`：只读兜底；任务不符合以上专用类型时使用，不承担代码实施。

除 `codey_worker` 和 `codey_visual_worker` 外，子代理默认只做探索、检索和核验，不改动文件。可写角色也只处理被明确授权且边界清晰的实现；方案取舍、关键代码复核和最终验证仍由主代理负责。

### 委派与验证

- 任务必须自包含，写清检索范围、具体问题、允许的改动范围和期望输出；精度重要时要求返回 `file:line`、符号名及必要关键原文。
- 多个相互独立的任务应在同一轮并发派发。派生时显式使用 `fork_turns = "none"`，不给子代理复制主线程历史。
- 子代理结果只是压缩后的线索。主代理沿其出处抽查关键部分，不要把已外包的材料完整重读一遍；但即将修改的确切代码和奠基性文档必须由主代理亲自完整读取。
- 每个子代理只用一轮，不复用、不追派；子代理不得继续派生其他子代理。
- 派发后立即等待全部子代理完成，再继续分析、命令或修改。子代理累计运行 10 分钟仍未完成时，检查状态并终止异常任务，必要时拆成更小任务重新派发。
"#;

/// Historical default text (sha1 4dfe3f11) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_7_3: &str = r#"## 子代理使用

本 AGENTS.md 明确要求主代理在适用任务中主动使用子代理；无需等待用户逐次点名。子代理用于把宽而重的检索、独立核验或边界清晰的实现从主线程中拆出，减少上下文污染、提高并行度并提供独立证据。

### 主动派发要求

除下述“直接处理”例外外，只要符合任一条件，必须派发至少一个合适的子代理：

- 需要先定位未知实现位置，或需要跨多个文件、目录、日志或文档检索；
- 可以拆成两个或更多相互独立的探索、核验或实现分支；
- 预计会产生大量搜索结果、日志、页面或其他外围材料，需要压缩后再判断；
- 存在边界清晰、可回滚且可测试的独立实现，可交给写入型角色处理。

不要因为用户没有明确要求子代理、主代理自己也能完成、任务已不在开头，或派发会增加一次工具调用而跳过。若多个条件同时成立，应优先拆成多个互不重叠的任务并在同一轮并发派发。

以下内容由主代理直接处理，不派子代理：已知位置的小文件或少量代码、即将修改的确切代码、奠基性文档，以及派发与复核成本明确不低于直接处理的单一事实。若任务只命中这些例外，不要为了满足数量而形式化派发。

### 任务类型路由

派生时必须按任务性质显式选择下列 `agent_type`，不要用模型名代替任务类型：

- `codey_quick_scan`：只读的快速定位、精确事实查找、重复性检查和低风险小范围检索。
- `codey_deep_research`：只读的跨文件、日志、代码或文档宽范围检索、归纳和架构探索。
- `codey_visual_analysis`：只读的截图、页面、GUI、PDF 等视觉证据分析，以及复杂探索或独立核验。
- `codey_worker`：可写的低到中等复杂度、边界清晰、可回滚且可测试的非视觉实现。
- `codey_visual_worker`：可写的页面、GUI、PDF 或其他依赖视觉证据与渲染验证的实现。
- `default`：只读兜底；任务不符合以上专用类型时使用，不承担代码实施。

除 `codey_worker` 和 `codey_visual_worker` 外，子代理默认只做探索、检索和核验，不改动文件。可写角色也只处理被明确授权且边界清晰的实现；方案取舍、关键代码复核和最终验证仍由主代理负责。

### 委派与验证

- 任务必须自包含，写清检索范围、具体问题、允许的改动范围和期望输出；精度重要时要求返回 `file:line`、符号名及必要关键原文。
- 多个相互独立的任务应在同一轮并发派发。派生时显式使用 `fork_turns = "none"`，不给子代理复制主线程历史。
- 子代理结果只是压缩后的线索。主代理沿其出处抽查关键部分，不要把已外包的材料完整重读一遍；但即将修改的确切代码和奠基性文档必须由主代理亲自完整读取。
- 每个子代理只用一轮，不复用、不追派；子代理不得继续派生其他子代理。
- 派发后立即等待全部子代理完成，再继续分析、命令或修改。子代理累计运行 10 分钟仍未完成时，检查状态并终止异常任务，必要时拆成更小任务重新派发。
"#;

/// Historical default text (sha1 a57b73a8) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_7_5: &str = r#"## 子代理使用

本 AGENTS.md 明确要求主代理在适用任务中主动使用子代理；无需等待用户逐次点名。子代理用于把宽而重的检索、独立核验或边界清晰的实现从主线程中拆出，减少上下文污染、提高并行度并提供独立证据。

### 主动派发要求

除下述“直接处理”例外外，只要符合任一条件，必须派发至少一个合适的子代理：

- 需要先定位未知实现位置，或需要跨多个文件、目录、日志或文档检索；
- 可以拆成两个或更多相互独立的探索、核验或实现分支；
- 预计会产生大量搜索结果、日志、页面或其他外围材料，需要压缩后再判断；
- 存在边界清晰、可回滚且可测试的独立实现，可交给写入型角色处理。

不要因为用户没有明确要求子代理、主代理自己也能完成、任务已不在开头，或派发会增加一次工具调用而跳过。若多个条件同时成立，应优先拆成多个互不重叠的任务并在同一轮并发派发。

以下内容由主代理直接处理，不派子代理：已知位置的小文件或少量代码、即将修改的确切代码、奠基性文档，以及派发与复核成本明确不低于直接处理的单一事实。若任务只命中这些例外，不要为了满足数量而形式化派发。

### 任务类型路由

派生时必须按任务性质显式选择下列 `agent_type`，不要用模型名代替任务类型：

- `codey_quick_scan`：只读的快速定位、精确事实查找、重复性检查和低风险小范围检索。
- `codey_deep_research`：只读的跨文件、日志、代码或文档宽范围检索、归纳和架构探索。
- `codey_visual_analysis`：只读的截图、页面、GUI、PDF 等视觉证据分析，以及复杂探索或独立核验。
- `codey_worker`：可写的低到中等复杂度、边界清晰、可回滚且可测试的非视觉实现。
- `codey_visual_worker`：可写的页面、GUI、PDF 或其他依赖视觉证据与渲染验证的实现。
- `default`：只读兜底；任务不符合以上专用类型时使用，不承担代码实施。

除 `codey_worker` 和 `codey_visual_worker` 外，子代理默认只做探索、检索和核验，不改动文件。可写角色也只处理被明确授权且边界清晰的实现；方案取舍、关键代码复核和最终验证仍由主代理负责。角色文件中的沙箱是默认值，实际权限仍受父任务当前权限模式约束。

### 委派与验证

- 任务必须自包含，写清检索范围、具体问题、允许的改动范围和期望输出；精度重要时要求返回 `file:line`、符号名及必要关键原文。
- 多个相互独立的任务应先完成同一批次的全部派发，再进入等待。派生时显式使用 `fork_turns = "none"`，不给子代理复制主线程历史。
- 子代理结果只是压缩后的线索。主代理沿其出处抽查关键部分，不要把已外包的材料完整重读一遍；但即将修改的确切代码和奠基性文档必须由主代理亲自完整读取。
- 每个子代理只用一轮，不复用、不追派；子代理不得继续派生其他子代理。
- 本轮计划的子代理全部派发后，在继续非协作分析、命令或修改前进入等待。等待返回 `MESSAGE` 或其他局部更新时，只可使用 `agents.*` 协作工具做必要的查看、转向或停止，然后继续等待；所有已派发子代理完成前不得恢复本地工作或结束任务。子代理累计运行 10 分钟仍未完成时，检查状态并终止异常任务，必要时拆成更小任务重新派发。
"#;

/// Historical default text (sha1 25a85246) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_PREVIOUS_V1: &str = r#"## 子代理使用

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

/// Historical default text (sha1 c7d980a1) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V5: &str = r#"## 子代理使用

子代理用于隔离宽而重的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理；只有预计收益明确高于派发、等待和复核成本时才使用子代理。不要为了形式上的分工而派生。

### 选择性委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查的成本不明显低于主代理直接完成的任务。

仅在符合以下至少一种情况，且任务不命中上述直接处理条件时派生：

- 至少有 2 个互不依赖的分支，每个分支预计需要至少 3 次实质工具调用，合计预计不少于 7 次；
- 实现位置未知，预计需要跨至少 2 个目录或检查至少 5 个候选文件，并且需要归纳而非一次搜索即可回答；
- 需要读取或比较大量日志、文档、页面等外围材料，预计会明显污染主线程上下文；
- 存在不在主代理关键路径上的独立实现，预计需要至少 6 次工具调用或较长时间，并且能给出互斥的文件 ownership；
- 高风险结论缺少确定性检查，需要一条真正独立的证据线；简单或已有确定性测试的任务不额外派 verifier。

不确定是否达到阈值时，直接处理。不得仅因为“可回滚、可测试”或“实现位置暂时未知”就派生。`codey_quick_scan` 只用于至少需要 3 次独立检索的定位任务；一两次搜索可完成的查询由主代理自己处理。默认最多派生 2 个子代理。超过 2 个必须同时满足：运行时已经显式配置更高并发；并且用户明确要求大规模并行，或存在 3 个以上均达到上述阈值的独立分支。

### 任务类型路由

派生时必须按任务性质显式选择下列 `agent_type`，不要用模型名代替任务类型：

- `codey_quick_scan`：只读的多点快速定位、重复性检查和低风险事实检索。
- `codey_deep_research`：只读的跨文件、日志、代码或文档宽范围检索、归纳和架构探索。
- `codey_visual_analysis`：只读的截图、页面、GUI、PDF 等视觉证据分析；普通复杂检索仍使用深度检索。
- `codey_worker`：可写的低到中等复杂度、边界清晰、可回滚且可测试的非视觉实现。
- `codey_visual_worker`：可写的页面、GUI、PDF 或其他依赖视觉证据与渲染验证的实现。

`default` 只作为旧配置和遗漏类型的内部只读兼容兜底，主代理不得主动选择。除 `codey_worker` 和 `codey_visual_worker` 外，子代理默认只做探索、检索和核验，不改动文件。可写角色也只处理被明确授权且 ownership 互斥的实现；方案取舍、关键代码复核和最终验证仍由主代理负责。角色文件中的沙箱是默认值，实际权限仍受父任务当前权限模式约束。

### 任务胶囊与结果契约

- 派发消息只携带完成子任务必需的目标、范围、允许操作和输出契约，不复制整段对话。派生时显式使用 `fork_turns = "none"`。
- 要求子代理以 `status: completed | partial | blocked` 开头，只返回会影响决策的结论、最多 5 条关键证据以及明确的 gaps；精度重要时证据必须包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩后的线索。主代理沿出处抽查关键部分；即将修改的确切代码和奠基性文档仍由主代理亲自完整读取。
- 准确性优先使用测试、类型检查、渲染结果等确定性验收。多代理意见冲突时比较证据，不按多数票决定；只有高风险且无法确定性验证时才追加一条独立核验线。

### 汇合、异常与接管

- 本轮计划的独立子代理必须先在同一批全部派发，再进入等待；有活动子代理时只使用 `agents.*` 做必要协调，直至每个代理进入终态。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。某分支失败后默认由主代理接管；只有缩小或改变范围后仍独立满足委派阈值，才允许最多重派一次。
- 收到 `MESSAGE` 或不完整结果时只保存可用证据并继续等待，不把局部更新当完成。出现 `errored`、`failed`、`shutdown` 或 `not_found` 时保留可核验证据，随后由主代理接管未完成部分。
- `pending_init` 长时间无进展，或运行累计 10 分钟仍无终态时，先调用一次不带筛选的 `agents.list_agents` 对账；仍非终态则对对应代理调用一次 `agents.interrupt_agent`，随后继续等待其进入终态。禁止无限 wait、无限重试或静默启动替代代理。
- 用户新输入导致等待中断时，将原批次视为可能过时：先中断仍活动的代理并完成对账，忽略迟到的旧结果，再按新请求决定是否继续。协作工具不可用时不要循环调用不存在的工具，等待 Codey 的有界遗留状态恢复。
"#;

/// Historical default text (sha1 5e4e75d9) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_7_8: &str = r#"## 子代理使用

子代理用于隔离宽而重的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理；只有预计收益明确高于派发、等待和复核成本时才使用子代理。不要为了形式上的分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

仅在任务不命中上述例外，且符合以下至少一种理由时派生：`multi_lookup` 表示快速定位至少需要 3 次独立检索；`parallel` 表示至少 2 个互不依赖的分支、每支至少 3 次实质调用且合计至少 7 次；`breadth` 表示需要归纳至少 2 个目录或 5 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。不确定时直接处理。

默认最多派生 2 个子代理。超过 2 个还必须有 3 个以上真正独立的分支，或用户明确要求大规模并行；不要用 verifier 重复已有确定性测试。每轮采用可恢复成本点预算：快速定位和 `default` 为 1 点，深度/视觉分析为 2 点，写入角色为 3 点；普通上限 8 点，明确的大批并行最高 12 点。该账本衡量派生尝试、角色成本和时限，不冒充上游未提供的精确 token 或费用。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V1={"id":"task_name","why":"breadth","calls":4,"files":5,"dirs":2,"visual":false,"read":[],"write":[],"checks":[]}`

`id` 必须等于 `task_name`；按实际理由填写 `branch_calls`、`large` 或 `risk`。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径。只声明本任务确实会使用的稳定路径；读取可以留空，写入角色的 `write` 不得为空。写入角色还必须提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。运行时会在派生前校验规模、角色能力、预算以及 read/write 父子路径冲突；重复任务 ID、越权写入和重叠 ownership 会被拒绝。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有结构化 `exit_code = 0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待；有活动子代理时只使用 `agents.*` 协调，直至每个代理进入终态。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍满足委派阈值，才允许使用新任务 ID 最多重派一次。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态则只中断一次并继续等待。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先中断活动代理并完成对账，忽略迟到结果。预算账本和未清偿验收债按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 9fd7ba01) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V7: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。默认最多派生 2 个子代理；超过 2 个还必须有 3 个以上真正独立的分支，或用户明确要求大规模并行。不要用 verifier 重复已有确定性测试。

每轮采用可恢复成本点预算：快速定位和 `default` 为 1 点，深度/视觉分析为 2 点，写入角色为 3 点；普通上限 8 点，明确的大批并行最高 12 点。该账本衡量派生尝试、角色成本和时限，不冒充上游未提供的精确 token 或费用。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V1={"id":"task_name","why":"breadth","calls":3,"files":4,"dirs":2,"visual":false,"read":[],"write":[],"checks":[]}`

`id` 必须等于 `task_name`；按实际情况填写 `calls`、`files`、`dirs`、`branch_calls`、`large` 或 `risk`，这些规模字段用于主代理路由说明而不是硬拒绝。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径。只声明本任务确实会使用的稳定路径；读取可以留空，写入角色的 `write` 不得为空。写入角色还必须提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。运行时会校验理由与角色兼容性、角色能力、预算以及 read/write 父子路径冲突，但不会因 calls/files/dirs 估算偏小拒绝有效契约；重复任务 ID、越权写入和重叠 ownership 仍会被拒绝。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有结构化 `exit_code = 0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待；有活动子代理时只使用 `agents.*` 协调，直至每个代理进入终态。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态则只中断一次并继续等待。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先中断活动代理并完成对账，忽略迟到结果。预算账本和未清偿验收债按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 b1bcce67) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_7_9: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。默认最多派生 2 个子代理；超过 2 个还必须有 3 个以上真正独立的分支，或用户明确要求大规模并行。不要用 verifier 重复已有确定性测试。

每轮采用可恢复成本点预算：快速定位和 `default` 为 1 点，深度/视觉分析为 2 点，写入角色为 3 点；普通上限 8 点。只有 `why: "parallel"` 且提供真实的 `branch_calls` 时，才按独立分支数自适应提高预算，最高 12 点；`user_requested` 只表达用户确实要求委派，不会自行放大预算。该账本衡量派生尝试、角色成本和时限，不冒充上游未提供的精确 token 或费用。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"read":[],"write":[],"checks":[]}`

`id` 必须等于 `task_name`。只有 `why: "parallel"` 时才填写 `branch_calls`，用数组记录各独立分支预计的实质调用数，例如 `"branch_calls":[3,3]`；其他路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径。只声明本任务确实会使用的稳定路径；读取可以留空，写入角色的 `write` 不得为空。写入角色还必须提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。运行时会校验理由与角色兼容性、角色能力、预算以及 read/write 父子路径冲突；重复任务 ID、越权写入和重叠 ownership 仍会被拒绝。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待；有活动子代理时只使用 `agents.*` 协调，直至每个代理进入终态。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态则只中断一次并继续等待。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先中断活动代理并完成对账，忽略迟到结果。预算账本和未清偿验收债按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 38056e8a) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_8_0: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。默认最多派生 2 个子代理；超过 2 个还必须有 3 个以上真正独立的分支，或用户明确要求大规模并行。不要用 verifier 重复已有确定性测试。

每轮采用可恢复成本点预算：快速定位和 `default` 为 1 点，深度/视觉分析为 2 点，写入角色为 3 点；普通上限 8 点。只有 `why: "parallel"` 且提供真实的 `branch_calls` 时，才按独立分支数自适应提高预算，最高 12 点；`user_requested` 只表达用户确实要求委派，不会自行放大预算。该账本衡量派生尝试、角色成本和时限，不冒充上游未提供的精确 token 或费用。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"read":[],"write":[],"checks":[]}`

`id` 必须等于 `task_name`。只有 `why: "parallel"` 时才填写 `branch_calls`，用数组记录各独立分支预计的实质调用数，例如 `"branch_calls":[3,3]`；其他路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径。只声明本任务确实会使用的稳定路径；读取可以留空，写入角色的 `write` 不得为空。写入角色还必须提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。运行时会校验理由与角色兼容性、角色能力、预算以及 read/write 父子路径冲突；重复任务 ID、越权写入和重叠 ownership 仍会被拒绝。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待；有活动子代理时只使用 `agents.*` 协调，直至每个代理进入终态。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态则只中断一次并继续等待。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先中断活动代理并完成对账，忽略迟到结果。预算账本和未清偿验收债按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 19d71237) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V10: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。默认最多派生 2 个子代理；超过 2 个还必须有 3 个以上真正独立的分支，或用户明确要求大规模并行。不要用 verifier 重复已有确定性测试。

每轮采用可恢复成本点预算：快速定位和 `default` 为 1 点，深度/视觉分析为 2 点，写入角色为 3 点；普通上限 8 点。只有 `why: "parallel"` 且提供真实的 `branch_calls` 时，才按独立分支数自适应提高预算，最高 12 点；`user_requested` 只表达用户确实要求委派，不会自行放大预算。该账本衡量派生尝试、角色成本和时限，不冒充上游未提供的精确 token 或费用。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`。只有 `why: "parallel"` 时才填写 `branch_calls`，用数组记录各独立分支预计的实质调用数，例如 `"branch_calls":[3,3]`；其他路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径。只声明本任务确实会读取和写入的稳定路径；省略 `root` 时采用 Hook 工作目录，空 `read` 对只读角色表示该 root、对写入角色表示其 `write` 范围。V2 契约必须显式声明 `files.read`；写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。worker 只有额外声明 `command.execute` 且 `write` 覆盖完整 root 时才能运行通用 shell。子代理网络能力不开放，契约不能自行声明或放宽。路径读取和写入都会按解析现存祖先后的物理路径校验；未绑定 attempt 不获得数据或副作用权限。运行时还会校验工具的完整可信名称、理由、角色、能力、预算及 read/write 冲突；重复任务 ID、越权访问和重叠 ownership 会被拒绝。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；同一根 turn 可继续完成本批派发与必要协调，child turn 或缺失/不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次并继续等待，否则依赖 Stop 的受控恢复路径 fence 遗留 attempt。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先完成对账；只有可信根 turn 绑定仍有效时才中断活动代理，否则让受控恢复路径接管并忽略迟到结果。预算账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 fb95fc9d) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V11: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。已确认的纯只读批次最多同时运行 3 个子代理；批次包含写入型或身份未确认的代理时，最多同时运行 2 个。不要用 verifier 重复已有确定性测试。

Codey 不再按角色成本点、每批尝试次数、批次数或根回合累计尝试数设置派发预算上限。编排账本只记录生命周期、批次决策、验收债和恢复所需元数据；并发限制只约束同时运行数量，不限制后续批次或累计派发次数，长期 Goal 任务也可以在同一根回合继续多个批次。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`；路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径；绝对 claim 可以位于 `root` 之外，跨多个工作树时分别声明即可。`root`、`read` 和 `write` 是冲突协调、ownership 与审计元数据，应尽量准确描述预计范围，但不是 Codey 的运行时文件路径 ACL。省略 `root` 时采用有效的 Hook 工作目录；Hook 目录缺失或无效但 claim 已是绝对路径时仍可派发。空 `read` 会对只读角色以 root、对写入角色以其 `write` 作为默认协调 claim，但不会收窄 Codex 原生可访问范围。V2 契约必须显式声明 `files.read`；写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。worker 额外声明 `command.execute` 后可以运行通用 shell，不要求 `write` 覆盖完整 root。子代理网络能力不开放，契约不能自行声明或放宽。未绑定 attempt 不获得数据或副作用权限；只读角色即使出现异常或旧账本也不能取得写能力。运行时还会校验工具的完整可信名称、理由、角色能力、角色感知并发上限及 read/write 冲突；重复任务 ID 和重叠 ownership 会被拒绝。

Codey 在 attempt 身份和角色 capability 校验通过后，不再解析、比较或拒绝读取、写入和命令中的目标路径，也不要求 child `cwd` 与契约 `root` 相同。子代理实际能访问哪些工作树、兄弟目录或外部路径，完全由其继承的 Codex sandbox、approval policy、permission profile 和 writable roots 决定；原生权限拒绝时按 Codex 的正常流程处理，不应通过修改 Codey 契约冒充权限提升。验证命令仍由根代理执行机械验收。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；同一根 turn 可继续完成本批派发与必要协调，child turn 或缺失/不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次。成功的 `agents.interrupt_agent` 表示根代理永久放弃该 attempt，Codey 会立即 fence 并核销本地活动状态；不要再等待或追派该 target，直接接管。中断失败或目标无法唯一匹配时才继续对账，并依赖 Stop 的受控恢复路径。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先完成对账；只有可信根 turn 绑定仍有效时才中断活动代理。中断成功后立即接管并忽略迟到结果，不再等待上游把 `interrupted` 另行改成终态；否则让受控恢复路径接管。编排账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 e359cbe5) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_8_5: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。已确认的纯只读批次最多同时运行 3 个子代理；批次包含写入型或身份未确认的代理时，最多同时运行 2 个。不要用 verifier 重复已有确定性测试。

Codey 不再按角色成本点、每批尝试次数、批次数或根回合累计尝试数设置派发预算上限。编排账本只记录生命周期、批次决策、验收债和恢复所需元数据；并发限制只约束同时运行数量，不限制后续批次或累计派发次数，长期 Goal 任务也可以在同一根回合继续多个批次。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`；路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径；绝对 claim 可以位于 `root` 之外，跨多个工作树时分别声明即可。`root`、`read` 和 `write` 是冲突协调、ownership 与审计元数据，应尽量准确描述预计范围，但不是 Codey 的运行时文件路径 ACL。省略 `root` 时采用有效的 Hook 工作目录；Hook 目录缺失或无效但 claim 已是绝对路径时仍可派发。空 `read` 会对只读角色以 root、对写入角色以其 `write` 作为默认协调 claim，但不会收窄 Codex 原生可访问范围。V2 契约必须显式声明 `files.read`；写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。worker 额外声明 `command.execute` 后可以运行通用 shell，不要求 `write` 覆盖完整 root。受信任的 Network 类网页搜索、打开和截图属于只读检索，活动 attempt 声明 `files.read` 即可使用，不要求额外网络 capability，也不会因此获得 shell 或本地写入；实际联网仍受 Codex 原生网络与审批设置约束。未绑定 attempt 不获得数据或副作用权限；只读角色即使出现异常或旧账本也不能取得写能力。运行时还会校验工具的完整可信名称、理由、角色能力、角色感知并发上限及 read/write 冲突；重复任务 ID 和重叠 ownership 会被拒绝。

Codey 在 attempt 身份和角色 capability 校验通过后，不再解析、比较或拒绝读取、写入和命令中的目标路径，也不要求 child `cwd` 与契约 `root` 相同。子代理实际能访问哪些工作树、兄弟目录或外部路径，完全由其继承的 Codex sandbox、approval policy、permission profile 和 writable roots 决定；原生权限拒绝时按 Codex 的正常流程处理，不应通过修改 Codey 契约冒充权限提升。验证命令仍由根代理执行机械验收。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；同一根 turn 可继续完成本批派发与必要协调，child turn 或缺失/不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次。成功的 `agents.interrupt_agent` 表示根代理永久放弃该 attempt，Codey 会立即 fence 并核销本地活动状态；不要再等待或追派该 target，直接接管。中断失败或目标无法唯一匹配时才继续对账，并依赖 Stop 的受控恢复路径。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先完成对账；只有可信根 turn 绑定仍有效时才中断活动代理。中断成功后立即接管并忽略迟到结果，不再等待上游把 `interrupted` 另行改成终态；否则让受控恢复路径接管。编排账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 f90cc8a9) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V11B: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。已确认的纯只读批次最多同时运行 3 个子代理；批次包含写入型或身份未确认的代理时，最多同时运行 2 个。不要用 verifier 重复已有确定性测试。

Codey 不再按角色成本点、每批尝试次数、批次数或根回合累计尝试数设置派发预算上限。编排账本只记录生命周期、批次决策、验收债和恢复所需元数据；并发限制只约束同时运行数量，不限制后续批次或累计派发次数，长期 Goal 任务也可以在同一根回合继续多个批次。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`；路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径；绝对 claim 可以位于 `root` 之外，跨多个工作树时分别声明即可。`root`、`read` 和 `write` 是冲突协调、ownership 与审计元数据，应尽量准确描述预计范围，但不是 Codey 的运行时文件路径 ACL。省略 `root` 时采用有效的 Hook 工作目录；Hook 目录缺失或无效但 claim 已是绝对路径时仍可派发。空 `read` 会对只读角色以 root、对写入角色以其 `write` 作为默认协调 claim，但不会收窄 Codex 原生可访问范围。V2 契约必须显式声明 `files.read`；写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`。worker 额外声明 `command.execute` 后可以运行通用 shell，不要求 `write` 覆盖完整 root。子代理网络能力不开放，契约不能自行声明或放宽。未绑定 attempt 不获得数据或副作用权限；只读角色即使出现异常或旧账本也不能取得写能力。运行时还会校验工具的完整可信名称、理由、角色能力、角色感知并发上限及 read/write 冲突；重复任务 ID 和重叠 ownership 会被拒绝。

Codey 在 attempt 身份和角色 capability 校验通过后，不再解析、比较或拒绝读取、写入和命令中的目标路径，也不要求 child `cwd` 与契约 `root` 相同。子代理实际能访问哪些工作树、兄弟目录或外部路径，完全由其继承的 Codex sandbox、approval policy、permission profile 和 writable roots 决定；原生权限拒绝时按 Codex 的正常流程处理，不应通过修改 Codey 契约冒充权限提升。验证命令仍由根代理执行机械验收。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；活动批次期间收到官方 `UserPromptSubmit` 时，会把该可信根事件的 `turn_id` 重新绑定到新一轮，并注入本段对账规则。child turn 或缺失/不匹配的 turn 仍只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。只有账本与活动 marker 共同证明全部剩余 attempt 均已绑定且仅具备 `files.read` 时，可信根代理才可在汇合间隙使用 FastCtx 的 `inspect_local_file`、`grep` 或 `glob` 消化部分结果；写入、命令、网络、其他本地工具和结束任务继续阻断，完成有界读取后立即回到 wait/list。存在 writer、命令能力、身份缺失、marker/账本不一致或状态异常时，所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次。成功的 `agents.interrupt_agent` 回执若报告目标先前仍在活动，Codey 才把它作为根代理永久放弃并立即 fence；若回执报告目标已经成功、失败、超时或丢失，则保留该真实终态。响应中出现的任何目标身份都必须与请求 target 唯一一致，否则不释放门禁。不要再等待或追派已核销 target；中断失败或身份无法唯一匹配时继续对账，并依赖 Stop 的受控恢复路径。禁止无限 wait、无限重试或静默重派。
- 用户新输入优先于旧任务描述，但不等于自动取消全部子代理。先执行一次无筛选 `agents.list_agents` 对账；只有用户明确取消、替换或缩小的子任务，才对仍非终态的对应 target 执行一次中断。状态询问、补充材料或对其他分支的修改不得误停未被点名的代理。中断核销后忽略该已取消请求的迟到结果；其余分支继续正常汇合。编排账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 1d20ae13) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_9_0: &str = r#"## 子代理使用

子代理用于隔离较宽的上下文、并行处理真正独立的工作，或取得有价值的独立证据。默认由主代理直接处理短而明确的工作；当只读检索可以减少主线程上下文、或并行收益清晰时，应主动考虑子代理，不要为了形式分工而派生。

### 自适应委派

以下任务直接处理，不派子代理：

- 回答、解释、状态汇报、单一事实或一次精确查询；
- 用户已经给出文件、符号或确切代码位置，预计只涉及不超过 2 个小文件和 3 次本地工具调用；
- 即将修改的确切代码、奠基性文档，以及步骤彼此依赖、无法真正并行的任务；
- 派发、等待和抽查成本不明显低于直接处理的任务。

任务命中以下形状时应积极考虑委派：`multi_lookup` 表示至少 2 次彼此独立的定位或事实检索，优先使用快速只读角色；`parallel` 表示至少 2 个互不依赖的分支、每支预计至少 2 次实质调用且合计至少 5 次；`breadth` 表示需要归纳至少 2 个目录或 4 个候选文件；`context` 表示大量日志、文档或页面会明显污染主线程；`independent_work` 表示不在主代理关键路径上的独立写入实现预计至少 6 次调用；`high_risk` 表示高风险结论缺少确定性检查；`user_requested` 仅在用户明确要求子代理或大规模并行时使用。

这些数量只是软路由参考，不是运行时硬门槛。有明确的上下文隔离价值或并行收益时，略低于参考值也可以委派；收益不清晰时直接处理。只读探索应比写入实现更积极，写入型子代理仍保持较高门槛。已确认的纯只读批次最多同时运行 3 个子代理；批次包含写入型或身份未确认的代理时，最多同时运行 2 个。不要用 verifier 重复已有确定性测试。

Codey 不再按角色成本点、每批尝试次数、批次数或根回合累计尝试数设置派发预算上限。编排账本只记录生命周期、批次决策、验收债和恢复所需元数据；并发限制只约束同时运行数量，不限制后续批次或累计派发次数，长期 Goal 任务也可以在同一根回合继续多个批次。

### 任务类型与运行时契约

派生时按任务性质显式选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 只作为旧配置兼容兜底，主代理不得主动选择。只读角色不能声明写入；视觉角色必须声明视觉需求；写入角色必须给出互斥的路径 ownership 和机械验收命令。角色沙箱只是默认工作方式，实际权限仍受父任务约束。

每个 `agents.spawn_agent` 必须使用 `fork_turns="none"`，`task_name` 使用小写字母、数字或下划线，并把一行紧凑 JSON 作为 message 的最后一个非空行：

`CODEY_DELEGATION_V2={"id":"task_name","why":"breadth","visual":false,"root":"/absolute/workspace","read":["relevant/path"],"write":[],"capabilities":["files.read"],"checks":[]}`

`id` 必须等于 `task_name`；路由规模只在派发判断中说明，不写入运行时契约。资源路径使用绝对路径，或者给出绝对 `root` 后使用相对路径；绝对 claim 可以位于 `root` 之外，跨多个工作树时分别声明即可。`root`、`read` 和 `write` 是冲突协调、ownership 与审计元数据，应尽量准确描述预计范围，但不是 Codey 的运行时文件路径 ACL。普通明文或只读密文契约省略 `root` 时采用有效的 Hook 工作目录；Hook 目录缺失或无效但 claim 已是绝对路径时仍可派发。写入 sidecar 必须显式给出绝对 `root`，不能依赖 Hook 工作目录。空 `read` 会对只读角色以 root、对写入角色以其 `write` 作为默认协调 claim，但不会收窄 Codex 原生可访问范围。V2 契约必须显式声明 `files.read`；需要 Git 状态、差异或其他终端读取的任务额外声明 `command.execute`。`command.execute` 只表示允许调用命令工具，不代表获得文件写权限：只读角色可在 `write=[]`、`checks=[]` 时声明它，写入仍由角色配置及继承的 Codex sandbox、approval policy 和 permission profile 决定。写入角色还必须声明 `workspace.write`，且 `write` 不得为空，并提供 1–3 个精确检查，例如 `"checks":[{"id":"tests","cmd":"cargo test -p app"}]`；通用 shell 不要求 `write` 覆盖完整 root。派发 `codey_worker` 或 `codey_visual_worker` 前，先调用 `mcp__codey_subagent_control__prepare_delegation`，传入相同 `task_name`、`agent_type`、唯一 `preparation_id` 和同一份 V2 `contract` JSON；工具 accepted 后立即调用匹配的 `agents.spawn_agent`。上游把任务正文替换为不可读密文时，写入角色只能消费该 sidecar 建立 ownership 和验收债；没有匹配 sidecar 时必须交回主代理。只读兼容 reservation 会保留 `files.read` 与 `command.execute`，但仍不能形成写 ownership 或验收债。受信任的 Network 类网页搜索、打开和截图属于只读检索，活动 attempt 声明 `files.read` 即可使用，不要求额外网络 capability，也不会因此获得本地写入；实际联网仍受 Codex 原生网络与审批设置约束。未绑定 attempt 不获得数据或副作用权限；只读角色即使出现异常或旧账本也不能取得直接写入工具能力。运行时还会校验工具的完整可信名称、理由、角色能力、角色感知并发上限及 read/write 冲突；重复任务 ID 和重叠 ownership 会被拒绝。

`prepare_delegation` 是可信根控制面的本地授权，不是对加密任务正文的上游签名证明。它只允许同一 `turn_id`、同一批次和两分钟 TTL 内随后出现的同名、同角色、真实 `fork_turns="none"` 密文写派发单次消费，并从 PreToolUse 起连续绑定规范化后的绝对 root/read/write 范围；回合、批次或 runtime 改变、nonce 重放、Pre/Post 范围漂移或任一字段冲突都会 fail-closed。若上一批刚结算，prepare 会先消费已提交的 `spawn_next_batch` 授权，再把 sidecar 记入新批次。根代理本身仍负责保证 sidecar 契约与它提交给上游加密的任务语义一致；Codey 按 sidecar 强制建立 ownership、capabilities 与验收债，不会尝试猜测密文正文。

Codey 在 attempt 身份和角色 capability 校验通过后，不再解析、比较或拒绝读取、写入和命令中的目标路径，也不要求 child `cwd` 与契约 `root` 相同。子代理实际能访问哪些工作树、兄弟目录或外部路径，完全由其继承的 Codex sandbox、approval policy、permission profile 和 writable roots 决定；原生权限拒绝时按 Codex 的正常流程处理，不应通过修改 Codey 契约冒充权限提升。验证命令仍由根代理执行机械验收。

### 任务胶囊与验收

- 派发消息只携带完成子任务必需的目标、范围、允许操作、ownership 和输出契约，不复制整段对话。
- 子代理首行必须是 `status: completed | partial | blocked`，只返回会影响决策的结论、最多 5 条关键证据和明确 gaps；关键证据包含 `file:line`、符号名或可复核链接。
- 子代理结果只是压缩线索。主代理沿出处抽查；即将修改的确切代码和奠基性文档仍由主代理完整读取。
- 写入型子代理的自报测试结果不算机械验收。所有子代理进入终态后，主代理必须按 Stop 门禁给出的 `# codey-accept:<task>:<check>` 标记逐字重跑契约命令；只有可信的退出状态 `0` 才清偿验收债。失败后修复并重跑，不能改写命令绕过。连续 3 次失败、连续 3 次 Stop 没有新证据，或持续 10 分钟无法验证时，门禁会把该项标记为“未验证”并在一次明确提示后释放；该批次只能以 `blocked` 结算，结算回执会持久保存未验证项，主代理必须向用户报告失败事实，不得声称验收通过。视觉质量仍需主代理查看实际渲染证据。
- 多代理证据冲突时比较出处，不按多数票决定；只有高风险且无法确定性验证时才追加独立核验线。

### 汇合、异常与接管

- 同一批独立任务先全部派发，再进入等待。Hook 会把本批首个根派生调用的可信 `turn_id` 绑定为编排主体；同一根 turn 可继续完成本批派发与必要协调，child turn 或缺失/不匹配的 turn 只能使用 `agents.wait_agent` 和无筛选的 `agents.list_agents` 对账，不能派生、追派、中断或操纵批次。所有代理进入终态前不得恢复普通本地工作。
- 每个子代理只用一轮，不复用、不追派，也不得继续派生。失败后默认由主代理接管；只有缩小或改变范围后仍值得独立委派，才允许使用新任务 ID 最多重派一次。
- 收到 `CODEY_SUBAGENT_DUPLICATE_TASK_ID` 时，不得重复旧 ID，也不得把拒绝当作完成后立即 Stop。先调用一次无筛选的 `agents.list_agents` 对账：原代理存在时等待其终态或消费已有终态结果，明确不存在时由主代理接管；只有任务范围确实改变且仍值得委派时，才可使用全新的 `task_name` 最多重试一次，并把 `CODEY_DELEGATION_V2.id` 同步改为完全相同的新值。
- `MESSAGE` 或不完整结果只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`running`、`pending_init` 和 `interrupted` 不是终态。
- `pending_init` 或运行累计 10 分钟无终态时，先用无筛选的 `agents.list_agents` 对账；仍无终态且根 turn 绑定有效时只中断一次。成功的 `agents.interrupt_agent` 表示根代理永久放弃该 attempt，Codey 会立即 fence 并核销本地活动状态；不要再等待或追派该 target，直接接管。中断失败或目标无法唯一匹配时才继续对账，并依赖 Stop 的受控恢复路径。禁止无限 wait、无限重试或静默重派。
- 用户新输入使旧批次失效时，先完成对账；只有可信根 turn 绑定仍有效时才中断活动代理。中断成功后立即接管并忽略迟到结果，不再等待上游把 `interrupted` 另行改成终态；否则让受控恢复路径接管。编排账本、未清偿验收债和未验证结算证据按运行代次与会话恢复；协作工具不可用时不要循环调用不存在的工具。
"#;

/// Historical default text (sha1 f4f4aa88) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_9_12: &str = r#"## 子代理使用

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

- 先派发彼此独立的任务，再等待所有活动 attempt 进入终态；期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 成功的 `agents.interrupt_agent` 会永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账：原代理存在则等待或消费结果，不存在则由根代理接管。只有任务范围实质改变时才用全新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界的 pending-init、超时和 Stop 恢复路径收敛。
"#;

/// Historical default text (sha1 423db054) reachable from old installs.
/// Kept only so the exact old default is upgraded to the current guidance.
pub(super) const LEGACY_SUBAGENT_GUIDANCE_V0_9_13: &str = r#"## 子代理使用

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
