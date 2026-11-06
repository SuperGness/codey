mod legacy;

const CONSERVATIVE_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

默认由主代理直接处理短而明确、步骤互相依赖或即将修改关键代码/文档的任务；不要为了形式分工而派生。只在独立并行工作、宽范围检索、上下文隔离或独立高风险证据确有收益时使用子代理。不超过 2 个小文件和 3 次本地工具调用的精确任务通常由主代理完成。

纯只读工作最多同时运行 3 个子代理；存在写入型或身份未确认的代理时最多同时运行 2 个。并发限制只约束同时运行数量，不限制后续派发次数。

### 派发

- 直接调用 `agents.spawn_agent`，按任务选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 仅兼容旧配置。`task_name` 只含小写字母、数字和下划线。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景，不复制整段对话，不附加 V1/V2 契约、sidecar、checks 或其他尾行协议。
- 只读角色获得 `files.read`；写入角色获得 `command.execute`、`files.read` 和 `workspace.write`。写入角色暂按当前工作区建立互斥锁；实际文件与网络权限仍由 Codex 原生 sandbox、approval policy、permission profile 和 writable roots 决定。

### 返回与验收

- 每个子代理默认执行一轮且不得继续派生；仅对已绑定、仍在运行且未被 fence 的 attempt 使用 `followup_task`，每个 attempt 最多追加 3 轮。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收；Codey 不再创建逐任务机械验收债或强制验收命令。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，按下一个计划任务的角色重新计算并发上限；存在空余槽位时立即使用新 `task_name` 补位，否则继续等待。所有计划任务均已派发后，继续等待剩余活动 attempt 结算。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 成功的 `agents.interrupt_agent` 会永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账：原代理存在则等待或消费结果，不存在则由根代理接管。只有任务范围实质改变时才用全新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界的 pending-init、超时和 Stop 恢复路径收敛。
"#;

const ROLE_RESTRICTED_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。

纯只读工作最多同时运行 3 个子代理；存在写入型或身份未确认的代理时最多同时运行 2 个。并发限制只约束同时运行数量，不限制后续派发次数。

### 派发

- 按当前客户端提供的工具接口调用 `agents.spawn_agent`；原生接口直接调用，客户端明确提供工具目录和专用转发入口时，使用该入口及目录中的准确名称和参数。不要把专用转发入口当作 JavaScript 聚合执行器，也不要因缺少同名直接接口就忽略目录中可用的工具。按任务选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 仅兼容旧配置。`task_name` 只含小写字母、数字和下划线。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景，不复制整段对话，不附加 V1/V2 契约、sidecar、checks 或其他尾行协议。
- 修改关键代码或文档时，可先派发独立的只读调查或核验；写入任务明确文件归属，避免重复调查或同时修改同一处。只读角色获得 `files.read`；写入角色获得 `command.execute`、`files.read` 和 `workspace.write`。写入角色暂按当前工作区建立互斥锁；实际权限仍由 Codex 原生 sandbox、approval policy、permission profile 和 writable roots 决定。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收；Codey 不再创建逐任务机械验收债或强制验收命令。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，按下一个计划任务的角色重新计算并发上限；存在空余槽位时立即使用新 `task_name` 补位，否则继续等待。所有计划任务均已派发后，继续等待剩余活动 attempt 结算。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 成功的 `agents.interrupt_agent` 会永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账：原代理存在则等待或消费结果，不存在则由根代理接管。只有任务范围实质改变时才用全新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界的 pending-init、超时和 Stop 恢复路径收敛。
"#;

const WRITE_AT_SPAWN_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。

最多同时运行 2 个子代理。各角色均可能写入，同一工作区采用互斥调度；并发限制只约束同时运行数量，不限制后续派发次数。

### 派发

- 按当前客户端提供的工具接口调用 `agents.spawn_agent`；原生接口直接调用，客户端明确提供工具目录和专用转发入口时，使用该入口及目录中的准确名称和参数。不要把专用转发入口当作 JavaScript 聚合执行器，也不要因缺少同名直接接口就忽略目录中可用的工具。按任务选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 仅兼容旧配置。`task_name` 只含小写字母、数字和下划线。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景，不复制整段对话，不附加 V1/V2 契约、sidecar、checks 或其他尾行协议。
- 修改关键代码或文档时，可先派发独立的调查或核验；写入任务明确文件归属，避免重复调查或同时修改同一处。角色只区分任务分工与模型，不限制工具类别；均获得 `command.execute`、`files.read`、`workspace.write` 和 `visual.inspect`。实际权限仍由 Codex 原生 sandbox、approval policy、permission profile 和 writable roots 决定。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收；Codey 不再创建逐任务机械验收债或强制验收命令。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，检查剩余并发额度；存在空余槽位时立即使用新 `task_name` 补位，否则继续等待。所有计划任务均已派发后，继续等待剩余活动 attempt 结算。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 成功的 `agents.interrupt_agent` 会永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账：原代理存在则等待或消费结果，不存在则由根代理接管。只有任务范围实质改变时才用全新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界的 pending-init、超时和 Stop 恢复路径收敛。
"#;

const WORKSPACE_WRITE_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。

最多同时运行 2 个子代理，同一工作区可并行调查与审查。角色只区分任务分工与模型，各角色工具能力一致。读取和检索不占写入资源；首次执行写入、命令或可能有副作用的工具时才登记工作区写入占用，重叠写入互斥，持续到该 attempt 结算。审查期间文件可能被其他任务修改，结论须注明所依据的代码版本或内容，并在汇合后复核。

### 派发

- 按当前客户端提供的工具接口调用 `agents.spawn_agent`；原生接口直接调用，客户端明确提供工具目录和专用转发入口时，使用该入口及目录中的准确名称和参数。不要通过 JavaScript 聚合执行器调用协作工具。按任务选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 仅兼容旧配置。`task_name` 只含小写字母、数字和下划线。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景。独立审查明确要求只读取和回报，写入任务明确文件归属；任务正文不构成资源隔离或权限证明。各角色均获得 `command.execute`、`files.read`、`workspace.write` 和 `visual.inspect`，实际权限仍由 Codex 原生权限决定。
- `CODEY_SUBAGENT_WORKSPACE_BUSY` 只表示本次工具需要的写入资源已被占用，子代理可继续读取和回报；不要反复重试、换工具绕过或把未执行的修改当作完成，由根代理在占用释放后安排修改。命令与未归类工具可能有副作用，按写入协调；调查优先使用已确认的读取工具。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，检查剩余并发额度并使用新 `task_name` 补位。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 仅对已绑定、仍在运行且未被 fence 的 attempt 使用 `followup_task`，每个 attempt 最多追加 3 轮。成功中断永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账；原代理不存在则由根代理接管，只有任务范围实质改变时才用新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界恢复路径收敛。
"#;

const COMMAND_WORKSPACE_CLAIM_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。

最多同时运行 2 个子代理，同一工作区允许并行调查、审查和修改不同文件。角色只区分任务分工与模型，各角色工具能力一致。读取和检索不占写入资源；原生补丁、单次明确补丁调用及 FastCtx 单文件替换按实际目标路径协调，重命名同时占用源文件与目标文件。命令、目录替换及无法可靠解析的工具保留工作区占用；目标路径无法确认时保守互斥。占用持续到该 attempt 结算，冲突工具不执行，多文件编辑整体取得占用后才放行。审查结论须注明依据的代码版本或内容，并在汇合后复核。

### 派发

- 按当前客户端提供的工具接口调用 `agents.spawn_agent`；原生接口直接调用，客户端明确提供工具目录和专用转发入口时，使用该入口及目录中的准确名称和参数。不要通过 JavaScript 聚合执行器调用协作工具。按任务选择 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker` 或 `codey_visual_worker`；`default` 仅兼容旧配置。`task_name` 只含小写字母、数字和下划线。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景。独立审查明确要求只读取和回报，写入任务明确文件归属；任务正文不构成资源隔离或权限证明。各角色均获得 `command.execute`、`files.read`、`workspace.write` 和 `visual.inspect`，实际权限仍由 Codex 原生权限决定。
- `CODEY_SUBAGENT_WORKSPACE_BUSY` 表示本次编辑涉及的文件或工作区被占用，子代理可继续读取、回报或修改不冲突的文件；不要反复重试、换工具绕过或把未执行的修改当作完成。由根代理在占用释放后安排剩余修改，重新读取相关文件并核对内容。任务正文与自报文件范围不能缩小工具实际占用；调查优先使用已确认的读取工具，范围较大的命令由根代理汇合后执行。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，检查剩余并发额度并使用新 `task_name` 补位。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 仅对已绑定、仍在运行且未被 fence 的 attempt 使用 `followup_task`，每个 attempt 最多追加 3 轮。成功中断永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账；原代理不存在则由根代理接管，只有任务范围实质改变时才用新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界恢复路径收敛。
"#;

const INHERITED_FORK_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。

最多同时运行 2 个子代理，同一工作区允许并行调查、审查和修改。角色只区分任务分工与模型，各角色工具能力一致。子代理的实际访问由 Codex 原生沙箱、审批策略和可写根目录约束。同一文件的并发修改容易互相覆盖，派发并行写入任务时应明确文件归属，修改前重新读取文件。多个写入者必须同时修改同一片代码时，改为串行执行或为每个写入者准备独立 worktree，由 Git 在合并阶段显式暴露冲突；审查结论须注明依据的代码版本或内容，并在汇合后复核。

### 派发

- 按当前客户端提供的工具接口调用 `agents.spawn_agent`；原生接口直接调用，客户端明确提供工具目录和专用转发入口时，使用该入口及目录中的准确名称和参数。不要通过 JavaScript 聚合执行器调用协作工具。`task_name` 只含小写字母、数字和下划线。
- 按任务性质选择角色：`codey_quick_scan` 用于小范围只读定位与事实检索；`codey_deep_research` 用于跨文件、跨目录的检索与架构归纳；`codey_visual_analysis` 用于视觉证据观察和前端改动的独立核验；`codey_visual_worker` 用于界面、布局、样式与交互实现；`codey_worker` 用于后端、数据、算法、命令行和脚本逻辑；`default` 仅兼容旧配置。改动用户可见界面的任务属于 `codey_visual_worker`，改派给 `codey_worker` 会丢失渲染验证。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景。独立审查明确要求只读取和回报，写入任务明确文件归属；任务正文不构成资源隔离或权限证明。各角色均获得 `command.execute`、`files.read`、`workspace.write` 和 `visual.inspect`，实际权限仍由 Codex 原生权限决定。
- 派发并行写入任务前确认文件归属不重叠；同一文件可能互相覆盖时，改派到其他文件或等前一个 attempt 结算。确实需要同时修改同一片代码时，改为串行执行或为每个写入者准备独立 worktree。任务正文与自报文件范围不构成额外权限，实际权限始终由 Codex 原生权限决定。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，检查剩余并发额度并使用新 `task_name` 补位。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 仅对已绑定、仍在运行且未被 fence 的 attempt 使用 `followup_task`，每个 attempt 最多追加 3 轮。成功中断永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账；原代理不存在则由根代理接管，只有任务范围实质改变时才用新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界恢复路径收敛。
"#;

const RETRIEVAL_NO_FORK_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。

最多同时运行 2 个子代理，同一工作区允许并行调查、审查和修改。角色只区分任务分工与模型，各角色工具能力一致。子代理的实际访问由 Codex 原生沙箱、审批策略和可写根目录约束。同一文件的并发修改容易互相覆盖，派发并行写入任务时应明确文件归属，修改前重新读取文件。多个写入者必须同时修改同一片代码时，改为串行执行或为每个写入者准备独立 worktree，由 Git 在合并阶段显式暴露冲突；审查结论须注明依据的代码版本或内容，并在汇合后复核。

### 派发

- 按当前客户端提供的工具接口调用 `agents.spawn_agent`；原生接口直接调用，客户端明确提供工具目录和专用转发入口时，使用该入口及目录中的准确名称和参数。不要通过 JavaScript 聚合执行器调用协作工具。`task_name` 只含小写字母、数字和下划线。
- 按任务性质选择角色：`codey_quick_scan` 用于小范围只读定位与事实检索；`codey_deep_research` 用于跨文件、跨目录的检索与架构归纳；`codey_visual_analysis` 用于视觉证据观察和前端改动的独立核验；`codey_visual_worker` 用于界面、布局、样式与交互实现；`codey_worker` 用于后端、数据、算法、命令行和脚本逻辑；`default` 仅兼容旧配置。改动用户可见界面的任务属于 `codey_visual_worker`，改派给 `codey_worker` 会丢失渲染验证。
- 派发 `codey_quick_scan`、`codey_deep_research` 或 `codey_visual_analysis` 时传 `fork_turns: "none"`，并在 `message` 中写全子代理需要的路径、符号、约束和交付格式；其他角色可省略该参数，按需继承会话历史。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景。独立审查明确要求只读取和回报，写入任务明确文件归属；任务正文不构成资源隔离或权限证明。各角色均获得 `command.execute`、`files.read`、`workspace.write` 和 `visual.inspect`，实际权限仍由 Codex 原生权限决定。
- 派发并行写入任务前确认文件归属不重叠；同一文件可能互相覆盖时，改派到其他文件或等前一个 attempt 结算。确实需要同时修改同一片代码时，改为串行执行或为每个写入者准备独立 worktree。任务正文与自报文件范围不构成额外权限，实际权限始终由 Codex 原生权限决定。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，检查剩余并发额度并使用新 `task_name` 补位。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 仅对已绑定、仍在运行且未被 fence 的 attempt 使用 `followup_task`，每个 attempt 最多追加 3 轮。成功中断永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账；原代理不存在则由根代理接管，只有任务范围实质改变时才用新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界恢复路径收敛。
"#;

const EARLY_DELEGATION_SUBAGENT_GUIDANCE: &str = r#"## 子代理使用

主动识别可独立推进的检索、实现和核验任务；存在明确分工或上下文隔离收益时，尽早使用子代理，无需用户逐次要求。先做确定边界所需的少量检查，再派发独立任务，不必等主代理完成同一调查后再分工。单步即可完成、步骤无法分离或委派没有实际收益的任务由主代理直接处理；不按文件数量或预计工具调用次数决定是否委派。

最多同时运行 2 个子代理，同一工作区允许并行调查、审查和修改。角色只区分任务分工与模型，各角色工具能力一致。子代理的实际访问由 Codex 原生沙箱、审批策略和可写根目录约束。同一文件的并发修改容易互相覆盖，派发并行写入任务时应明确文件归属，修改前重新读取文件。多个写入者必须同时修改同一片代码时，改为串行执行或为每个写入者准备独立 worktree，由 Git 在合并阶段显式暴露冲突；审查结论须注明依据的代码版本或内容，并在汇合后复核。

### 派发

- 按当前客户端提供的工具接口调用 `agents.spawn_agent`；原生接口直接调用，客户端明确提供工具目录和专用转发入口时，使用该入口及目录中的准确名称和参数。不要通过 JavaScript 聚合执行器调用协作工具。`task_name` 只含小写字母、数字和下划线。
- 按任务性质选择角色：`codey_quick_scan` 用于小范围只读定位与事实检索；`codey_deep_research` 用于跨文件、跨目录的检索与架构归纳；`codey_visual_analysis` 用于视觉证据观察和前端改动的独立核验；`codey_visual_worker` 用于界面、布局、样式与交互实现；`codey_worker` 用于后端、数据、算法、命令行和脚本逻辑；`default` 仅兼容旧配置。改动用户可见界面的任务属于 `codey_visual_worker`，改派给 `codey_worker` 会丢失渲染验证。
- 派发 `codey_quick_scan`、`codey_deep_research` 或 `codey_visual_analysis` 时传 `fork_turns: "none"`，并让 `message` 自包含：子代理需要的路径、符号、约束和交付格式，用户附件与截图的绝对路径，用户明确的偏好与已定决策，以及已查过或已排除的方向。任务依赖无法改写成文字或路径的对话内容时省略该参数；其他角色也可省略，按需继承会话历史。
- `message` 是唯一任务胶囊：写清目标、范围、允许操作、交付格式和必要背景。独立审查明确要求只读取和回报，写入任务明确文件归属；任务正文不构成资源隔离或权限证明。各角色均获得 `command.execute`、`files.read`、`workspace.write` 和 `visual.inspect`，实际权限仍由 Codex 原生权限决定。
- 派发并行写入任务前确认文件归属不重叠；同一文件可能互相覆盖时，改派到其他文件或等前一个 attempt 结算。确实需要同时修改同一片代码时，改为串行执行或为每个写入者准备独立 worktree。任务正文与自报文件范围不构成额外权限，实际权限始终由 Codex 原生权限决定。

### 返回与验收

- 每个子代理只执行一轮且不得继续派生。返回首行使用 `status: completed | partial | blocked`，正文只保留影响决策的结论、最多 5 条带 `file:line`/符号/链接的证据和明确 gaps；多代理证据冲突时比较出处。
- 子代理结果是候选产物，不是验收结论。所有代理结算后，由根代理结合用户要求、变更差异和必要的确定性检查统一验收；开发子代理留给主代理的全量测试和全仓库格式化在此时统一执行。

### 生命周期

- 先派发不超过当前并发上限的独立任务，再进入 wait/list。任一 attempt 终态或被成功中断并 fence 后，检查剩余并发额度并使用新 `task_name` 补位。活动 attempt 期间只使用必要的 `agents.*` 协作工具，普通本地工作和 Stop 仍受生命周期门禁限制。
- `MESSAGE` 只保存证据并继续等待。`completed`、`errored`、`error`、`failed`、`shutdown`、`not_found`、`FINAL_ANSWER` 和 `task_complete` 为终态；`pending_init`、`running`、`interrupted` 仍是非终态，除非根代理成功中断并永久放弃该 attempt。
- 仅对已绑定、仍在运行且未被 fence 的 attempt 使用 `followup_task`，每个 attempt 最多追加 3 轮。成功中断永久 fence 该 attempt；不要再等待或追派。重复 task ID 时只做一次无筛选 `agents.list_agents` 对账；原代理不存在则由根代理接管，只有任务范围实质改变时才用新 task ID 最多重派一次。
- 协作工具不可用时不要循环调用；依赖有界恢复路径收敛。
"#;

/// Root guidance Codey once appended to `developer_instructions`. Desktop
/// replaces that key per thread, so none is injected now; every version is
/// only removed from user config.
pub(crate) const SUBAGENT_GUIDANCE_VERSIONS: &[&str] = &[
    EARLY_DELEGATION_SUBAGENT_GUIDANCE,
    RETRIEVAL_NO_FORK_SUBAGENT_GUIDANCE,
    INHERITED_FORK_SUBAGENT_GUIDANCE,
    legacy::LEGACY_SUBAGENT_GUIDANCE_PRE_ROLE_TAXONOMY,
    COMMAND_WORKSPACE_CLAIM_SUBAGENT_GUIDANCE,
    WORKSPACE_WRITE_SUBAGENT_GUIDANCE,
    WRITE_AT_SPAWN_SUBAGENT_GUIDANCE,
    ROLE_RESTRICTED_SUBAGENT_GUIDANCE,
    legacy::DIRECT_SUBAGENT_GUIDANCE,
    CONSERVATIVE_SUBAGENT_GUIDANCE,
    legacy::DELEGATION_V2_GUIDANCE,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_7_1,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_7_3,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_7_5,
    legacy::LEGACY_SUBAGENT_GUIDANCE_PREVIOUS_V1,
    legacy::LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V5,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_7_8,
    legacy::LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V7,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_7_9,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_8_0,
    legacy::LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V10,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_8_2,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_8_3,
    legacy::LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V11,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_8_5,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_8_7,
    legacy::LEGACY_PREVIOUS_SUBAGENT_GUIDANCE_V11B,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_9_0,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_9_7,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_9_12,
    legacy::LEGACY_SUBAGENT_GUIDANCE_V0_9_13,
];

const PRE_INTERRUPT_FENCING_USAGE_HINT: &str = "\
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

const ROLE_BASED_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Dispatch up to the current concurrency limit from the \
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
interrupt permanently abandons and fences that attempt and settles it for the lifecycle ledger; \
do not wait for or follow up that target. Interrupt revokes child tool access before the provider call, \
but keeps the writer reservation until acknowledgement or terminal reconciliation. Queued followups \
may still start another turn; they cannot restore tool access. A failed interrupt does not release the \
reservation. Match evidence and write reports to the exact task and attempt; never transfer another \
attempt's no-change claim or infer a transport stall from elapsed time alone. If a wait times out or \
lacks per-agent terminal details, call unfiltered `agents.list_agents` before waiting again. Continue until \
all planned work has been spawned and every attempt is terminal or fenced. Then the root agent validates \
the combined result and either continues the work or finishes. While an attempt is active, Codey's gate \
blocks non-collaboration tools and Stop. If collaboration tools are unavailable, do not loop on an \
unregistered tool.";

const WRITE_AT_SPAWN_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Before waiting, dispatch independent tasks up to \
the concurrency limit of two only when their trusted native workspaces do not overlap; shared-workspace tasks run \
serially. Task text and claimed file ownership do not establish an isolated workspace. \
`CODEY_SUBAGENT_WORKSPACE_BUSY` means the workspace is occupied: wait for your own active task, \
or defer the affected work when another session owns it; do not repeatedly respawn into the occupied workspace. \
While any attempt is active, use only the relevant `agents.spawn_agent`, `agents.send_message`, \
`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. \
After a terminal or successfully fenced update, check remaining capacity and workspace availability \
before dispatching the next planned, unspawned task with a new `task_name`; otherwise return to `agents.wait_agent` \
with `timeout_ms: 30000`. `MESSAGE` and mailbox updates are not completion. Use `followup_task` only \
for a bound nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do \
not retry or wait for that target; take over or use a fresh `task_name` for a materially changed task. \
Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and \
`not_found` as terminal. A successful root interrupt permanently abandons and fences that attempt \
and settles it for the lifecycle ledger; do not wait for or follow up that target. Interrupt revokes \
child tool access before the provider call, but keeps the writer reservation until acknowledgement \
or terminal reconciliation. Queued followups may still start another turn; they cannot restore tool \
access. A failed interrupt does not release the reservation. Match evidence and write reports to the \
exact task and attempt; never transfer another attempt's no-change claim or infer a transport stall \
from elapsed time alone. If a wait times out or lacks per-agent terminal details, call unfiltered \
`agents.list_agents` before waiting again. Continue until all planned work has been performed and \
every attempt is terminal or fenced. Then the root agent validates the combined result and either \
continues the work or finishes. While an attempt is active, Codey's gate blocks non-collaboration \
tools and Stop. If collaboration tools are unavailable, do not loop on an unregistered tool.";

const WORKSPACE_WRITE_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Before waiting, dispatch independent tasks up to \
the concurrency limit of two; investigation and review may share a workspace. Roles have the same \
tool capabilities. Reading and searching do not claim writing resources. The first write, command, \
UI automation or unknown tool atomically claims workspace writing until the attempt settles; \
overlapping writes are exclusive. `CODEY_SUBAGENT_WORKSPACE_BUSY` denies that tool, not task startup: \
continue permitted reads and reporting, do not repeatedly retry or switch tools to bypass it, and \
let the root schedule deferred modifications after the writer settles. Review may see changing files; \
report the revision or content examined and recheck conclusions after all attempts settle. \
While any attempt is active, use only the relevant `agents.spawn_agent`, `agents.send_message`, \
`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. \
After a terminal or successfully fenced update, check remaining capacity before dispatching the next \
planned, unspawned task with a new `task_name`; otherwise return to `agents.wait_agent` with \
`timeout_ms: 30000`. `MESSAGE` and mailbox updates are not completion. Use `followup_task` only for \
a bound nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not \
retry or wait for that target; take over or use a fresh `task_name` for a materially changed task. \
Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and \
`not_found` as terminal. A successful root interrupt permanently abandons and fences that attempt \
and settles it for the lifecycle ledger; do not wait for or follow up that target. Interrupt revokes \
child tool access before the provider call, but keeps the writer reservation until acknowledgement \
or terminal reconciliation. Queued followups may still start another turn; they cannot restore tool \
access. A failed interrupt does not release the reservation. Match evidence and write reports to the \
exact task and attempt; never transfer another attempt's no-change claim or infer a transport stall \
from elapsed time alone. If a wait times out or lacks per-agent terminal details, call unfiltered \
`agents.list_agents` before waiting again. Continue until all planned work has been performed and \
every attempt is terminal or fenced. Then the root agent validates the combined result and either \
continues the work or finishes. While an attempt is active, Codey's gate blocks non-collaboration \
tools and Stop. If collaboration tools are unavailable, do not loop on an unregistered tool.";

const COMMAND_WORKSPACE_CLAIM_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Before waiting, dispatch independent tasks up to \
the concurrency limit of two; investigation, review and edits of different files may share a workspace. \
Roles have the same tool capabilities. Reading and searching do not claim writing resources. \
Native patches, exact single patch calls and native FastCtx single-file replacement atomically claim \
their complete target paths; renames claim both names. Commands, directory replacement and opaque tools \
retain workspace scope, and unresolved targets require conservative exclusion. Claims last until the \
attempt settles. Multi-file claims are all-or-nothing. Task prose cannot narrow actual tool scope. \
`CODEY_SUBAGENT_WORKSPACE_BUSY` denies that tool, not task startup: continue permitted reads or edits \
of non-conflicting files, do not repeatedly retry or switch tools to bypass it, and let the root schedule \
deferred modifications after the writer settles. Re-read deferred files before editing. The root should \
run broad commands after gathering child results. Review may see changing files; report the revision or \
content examined and recheck conclusions after all attempts settle. \
While any attempt is active, use only the relevant `agents.spawn_agent`, `agents.send_message`, \
`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. \
After a terminal or successfully fenced update, check remaining capacity before dispatching the next \
planned, unspawned task with a new `task_name`; otherwise return to `agents.wait_agent` with \
`timeout_ms: 30000`. `MESSAGE` and mailbox updates are not completion. Use `followup_task` only for \
a bound nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not \
retry or wait for that target; take over or use a fresh `task_name` for a materially changed task. \
Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and \
`not_found` as terminal. A successful root interrupt permanently abandons and fences that attempt \
and settles it for the lifecycle ledger; do not wait for or follow up that target. Interrupt revokes \
child tool access before the provider call, but keeps the writer reservation until acknowledgement \
or terminal reconciliation. Queued followups may still start another turn; they cannot restore tool \
access. A failed interrupt does not release the reservation. Match evidence and write reports to the \
exact task and attempt; never transfer another attempt's no-change claim or infer a transport stall \
from elapsed time alone. If a wait times out or lacks per-agent terminal details, call unfiltered \
`agents.list_agents` before waiting again. Continue until all planned work has been performed and \
every attempt is terminal or fenced. Then the root agent validates the combined result and either \
continues the work or finishes. While an attempt is active, Codey's gate blocks non-collaboration \
tools and Stop. If collaboration tools are unavailable, do not loop on an unregistered tool.";

const LIST_AFTER_TIMEOUT_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Before waiting, dispatch independent tasks up to \
the concurrency limit of two; investigation, review and edits may share a workspace. Roles have the same \
tool capabilities; native sandbox, approval policy, permission profile and writable roots bound what \
each tool can touch. Because concurrent edits to one file can overwrite each other, assign distinct files \
when dispatching parallel writers, and re-read a file before editing it. When two writers must change the \
same code, serialize them or give each an isolated worktree and let Git surface the conflict at merge time. \
Review may see changing files; \
report the revision or content examined and recheck conclusions after all attempts settle. \
While any attempt is active, use only the relevant `agents.spawn_agent`, `agents.send_message`, \
`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. \
After a terminal or successfully fenced update, check remaining capacity before dispatching the next \
planned, unspawned task with a new `task_name`; otherwise return to `agents.wait_agent` with \
`timeout_ms: 30000`. `MESSAGE` and mailbox updates are not completion. Use `followup_task` only for \
a bound nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not \
retry or wait for that target; take over or use a fresh `task_name` for a materially changed task. \
Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and \
`not_found` as terminal. A successful root interrupt permanently abandons and fences that attempt \
and settles it for the lifecycle ledger; do not wait for or follow up that target. Interrupt revokes \
child tool access before the provider call. Queued followups may still start another turn; they cannot \
restore tool access. Match evidence and write reports to the exact task and attempt; never transfer \
another attempt's no-change claim or infer a transport stall from elapsed time alone. If a wait times \
out or lacks per-agent terminal details, call unfiltered `agents.list_agents` before waiting again. \
Continue until all planned work has been performed and every attempt is terminal or fenced. Then the \
root agent validates the combined result and either continues the work or finishes. While an attempt \
is active, Codey's gate blocks non-collaboration tools and Stop. If collaboration tools are unavailable, \
do not loop on an unregistered tool.";

const WAIT_30S_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Before waiting, dispatch independent tasks up to \
the concurrency limit of two; investigation, review and edits may share a workspace. Roles have the same \
tool capabilities; native sandbox, approval policy, permission profile and writable roots bound what \
each tool can touch. Because concurrent edits to one file can overwrite each other, assign distinct files \
when dispatching parallel writers, and re-read a file before editing it. When two writers must change the \
same code, serialize them or give each an isolated worktree and let Git surface the conflict at merge time. \
Review may see changing files; \
report the revision or content examined and recheck conclusions after all attempts settle. \
While any attempt is active, use only the relevant `agents.spawn_agent`, `agents.send_message`, \
`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. \
After a terminal or successfully fenced update, check remaining capacity before dispatching the next \
planned, unspawned task with a new `task_name`; otherwise return to `agents.wait_agent` with \
`timeout_ms: 30000`. `MESSAGE` and mailbox updates are not completion. Use `followup_task` only for \
a bound nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not \
retry or wait for that target; take over or use a fresh `task_name` for a materially changed task. \
Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and \
`not_found` as terminal. A successful root interrupt permanently abandons and fences that attempt \
and settles it for the lifecycle ledger; do not wait for or follow up that target. Interrupt revokes \
child tool access before the provider call. Queued followups may still start another turn; they cannot \
restore tool access. Match evidence and write reports to the exact task and attempt; never transfer \
another attempt's no-change claim or infer a transport stall from elapsed time alone. A timed-out \
wait only means no new message arrived; call `agents.wait_agent` again directly. Call unfiltered \
`agents.list_agents` only when a wait completes without per-agent terminal details or Codey's gate shows \
that an attempt left the active list. \
Continue until all planned work has been performed and every attempt is terminal or fenced. Then the \
root agent validates the combined result and either continues the work or finishes. While an attempt \
is active, Codey's gate blocks non-collaboration tools and Stop. If collaboration tools are unavailable, \
do not loop on an unregistered tool.";

const WAIT_120S_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Before waiting, dispatch independent tasks up to \
the concurrency limit of two; investigation, review and edits may share a workspace. Roles have the same \
tool capabilities; native sandbox, approval policy, permission profile and writable roots bound what \
each tool can touch. Because concurrent edits to one file can overwrite each other, assign distinct files \
when dispatching parallel writers, and re-read a file before editing it. When two writers must change the \
same code, serialize them or give each an isolated worktree and let Git surface the conflict at merge time. \
Review may see changing files; \
report the revision or content examined and recheck conclusions after all attempts settle. \
Pass `fork_turns: \"none\"` when spawning `codey_quick_scan`, `codey_deep_research` or \
`codey_visual_analysis`, and make `message` self-contained with the paths, symbols, constraints and \
expected output the child needs; other roles may omit `fork_turns`. \
While any attempt is active, use only the relevant `agents.spawn_agent`, `agents.send_message`, \
`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. \
After a terminal or successfully fenced update, check remaining capacity before dispatching the next \
planned, unspawned task with a new `task_name`; otherwise return to `agents.wait_agent` with \
`timeout_ms: 120000`. `MESSAGE` and mailbox updates are not completion. Use `followup_task` only for \
a bound nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not \
retry or wait for that target; take over or use a fresh `task_name` for a materially changed task. \
Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and \
`not_found` as terminal. A successful root interrupt permanently abandons and fences that attempt \
and settles it for the lifecycle ledger; do not wait for or follow up that target. Interrupt revokes \
child tool access before the provider call. Queued followups may still start another turn; they cannot \
restore tool access. Match evidence and write reports to the exact task and attempt; never transfer \
another attempt's no-change claim or infer a transport stall from elapsed time alone. A timed-out \
wait only means no new message arrived; call `agents.wait_agent` again directly. Call unfiltered \
`agents.list_agents` only when a wait completes without per-agent terminal details or Codey's gate shows \
that an attempt left the active list. \
Continue until all planned work has been performed and every attempt is terminal or fenced. Then the \
root agent validates the combined result and either continues the work or finishes. While an attempt \
is active, Codey's gate blocks non-collaboration tools and Stop. If collaboration tools are unavailable, \
do not loop on an unregistered tool.";

const SELECTIVE_FORK_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools use the current \
client's declared tool interface. Native clients expose direct commentary tools; never call them through \
`functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a \
transport-only endpoint, use that documented endpoint with the exact catalog name and argument schema. \
A catalog-listed collaboration tool is available even without a same-named direct schema. Do not invent \
wrappers or bypass lifecycle and permission checks. Before waiting, dispatch independent tasks up to \
the concurrency limit of two; investigation, review and edits may share a workspace. Roles have the same \
tool capabilities; native sandbox, approval policy, permission profile and writable roots bound what \
each tool can touch. Because concurrent edits to one file can overwrite each other, assign distinct files \
when dispatching parallel writers, and re-read a file before editing it. When two writers must change the \
same code, serialize them or give each an isolated worktree and let Git surface the conflict at merge time. \
Review may see changing files; \
report the revision or content examined and recheck conclusions after all attempts settle. \
Pass `fork_turns: \"none\"` when spawning `codey_quick_scan`, `codey_deep_research` or \
`codey_visual_analysis`, and make `message` self-contained: the paths, symbols, constraints and \
expected output the child needs, absolute paths of user attachments and screenshots, the user's stated \
preferences and settled decisions, and what has already been checked or ruled out. Omit `fork_turns` \
when the task depends on conversation content that cannot be restated as text or paths; other roles \
may omit it too. \
While any attempt is active, use only the relevant `agents.spawn_agent`, `agents.send_message`, \
`agents.followup_task`, `agents.interrupt_agent`, `agents.list_agents`, or `agents.wait_agent`. \
After a terminal or successfully fenced update, check remaining capacity before dispatching the next \
planned, unspawned task with a new `task_name`; otherwise return to `agents.wait_agent` with \
`timeout_ms: 120000`. `MESSAGE` and mailbox updates are not completion. Use `followup_task` only for \
a bound nonterminal attempt. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not \
retry or wait for that target; take over or use a fresh `task_name` for a materially changed task. \
Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and \
`not_found` as terminal. A successful root interrupt permanently abandons and fences that attempt \
and settles it for the lifecycle ledger; do not wait for or follow up that target. Interrupt revokes \
child tool access before the provider call. Queued followups may still start another turn; they cannot \
restore tool access. Match evidence and write reports to the exact task and attempt; never transfer \
another attempt's no-change claim or infer a transport stall from elapsed time alone. A timed-out \
wait only means no new message arrived; call `agents.wait_agent` again directly. Call unfiltered \
`agents.list_agents` only when a wait completes without per-agent terminal details or Codey's gate shows \
that an attempt left the active list. \
Continue until all planned work has been performed and every attempt is terminal or fenced. Then the \
root agent validates the combined result and either continues the work or finishes. While an attempt \
is active, Codey's gate blocks non-collaboration tools and Stop. If collaboration tools are unavailable, \
do not loop on an unregistered tool.";

pub(crate) const ROOT_AGENT_COLLABORATION_USAGE_HINT: &str = r#####"Explorer's native sandbox is read-only. Reviewer uses workspace-write for tests, builds and render outputs and remains a potential writer; it must not repair source. The root may write only after the gate confirms every active Explorer's current native turn is read-only.

`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools are direct commentary tools in native clients; never call them through `functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a transport-only endpoint, use that endpoint with the exact catalog name and argument schema; a catalog-listed tool is available even without a same-named direct schema. Do not invent wrappers or bypass lifecycle and permission checks. Delegate only independent parts when parallel work, context isolation, or specialized evidence can reduce end-to-end time or preserve useful context after counting dispatch, task text, waiting and acceptance. Keep a clear bounded task in the root when several tool calls are enough to finish it. Never add classification or synthesis agents, split tasks to fill slots, or spawn just to fill slots. Always set `agent_type`. Use `codey_explorer` for multi-step read-only research, `codey_worker` for implementation, and `codey_reviewer` for independent verification of completed stable input. Assign distinct files to parallel writers and have them re-read a file before editing it. When writers must change the same files, serialize them or use an isolated worktree or copy when the work benefits from isolation; Git may expose merge conflicts. Pass `fork_turns: "none"` for every role and make `message` self-contained: goal, scope, paths, quick/medium/thorough boundary and stopping condition, constraints, expected output, absolute paths of user attachments and screenshots, the user's stated preferences and settled decisions, and what has already been checked or ruled out. Workers run checks for their changed scope, including rendered UI evidence when available. Reviewer checks the stated revision or diff and required behavior. The root spot-checks key and conflicting evidence plus integration and uncovered checks; do not repeat the whole investigation or every successful targeted check. Never put passwords, tokens or keys in `message`; keep work needing them in the root. Omit `fork_turns` only when the task depends on conversation content that cannot be restated as text or paths. Do not request interim progress: children message only for blockers or plan-changing facts, and final answers arrive through `agents.wait_agent`; use `agents.send_message` only for facts that change a child's task. When the gate confirms every active Explorer's current native turn is read-only, keep working on independent files, writes and commands included, and wait when you need its findings; tell an explorer if you edit files it is reading. Any other active child limits you to `agents.*` tools and the read tools `read_file`, `inspect_local_file`, `grep`, `glob`, and `view_image`, to prepare review, not redo delegated work. After followup, an unscoped final notification needs a current list_agents snapshot before accepting its outcome. Stop waits for every attempt. After a terminal or fenced update, dispatch the next planned, unspawned task with a new `task_name` if a slot is free; otherwise return to `agents.wait_agent` with `timeout_ms: 120000`. Call unfiltered `agents.list_agents` only when a wait completes without per-agent terminal details or Codey's gate shows that an attempt left the active list, or the gate requests a current-turn snapshot for an unscoped followup completion. `MESSAGE` and mailbox updates are not completion. Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and `not_found` as terminal. Use `agents.followup_task` to continue a child's own earlier work, even from a later turn: it keeps the child's context, so prefer it over a fresh spawn. Give one `codey_explorer` all related questions about an area and follow up with it on that area; spawn fresh only for unrelated work. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not retry or wait for that target; take over or use a fresh `task_name`. A successful root interrupt permanently abandons and fences that attempt, revokes its tool access and settles it for the lifecycle ledger; do not wait for or follow up that target. Match evidence to the exact task and attempt; never reuse another attempt's no-change claim or infer a stall from elapsed time alone. If a child cannot decrypt its task body, restate the complete task with `agents.send_message` exactly once; do not interrupt or respawn it, and take over if that fails. On `CODEY_SUBAGENT_DUPLICATE_TASK_ID`, call unfiltered `agents.list_agents` once: wait for the original if present, otherwise take over; only a materially changed task may retry once with a fresh `task_name`. If every spawn fails, take over; if collaboration tools are unavailable, do not loop on an unregistered tool. Continue until all planned work has been performed and every attempt is terminal or fenced, then validate the combined result and the integration or uncovered checks required by project risk before continuing or finishing."#####;

const PRE_SCOPED_COMPLETION_COLLABORATION_USAGE_HINT: &str = r#####"Explorer's native sandbox is read-only. Reviewer uses workspace-write for tests, builds and render outputs and remains a potential writer; it must not repair source. The root may write only after the gate confirms every active Explorer's current native turn is read-only.

`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools are direct commentary tools in native clients; never call them through `functions.exec` as a JavaScript aggregate. If a client explicitly exposes a tool catalog through a transport-only endpoint, use that endpoint with the exact catalog name and argument schema; a catalog-listed tool is available even without a same-named direct schema. Do not invent wrappers or bypass lifecycle and permission checks. Delegate only independent parts when parallel work, context isolation, or specialized evidence can reduce end-to-end time or preserve useful context after counting dispatch, task text, waiting and acceptance. Keep a clear bounded task in the root when several tool calls are enough to finish it. Never add classification or synthesis agents, split tasks to fill slots, or spawn just to fill slots. Always set `agent_type`. Use `codey_explorer` for multi-step read-only research, `codey_worker` for implementation, and `codey_reviewer` for independent verification of completed stable input. Assign distinct files to parallel writers and have them re-read a file before editing it. When writers must change the same files, serialize them or use an isolated worktree or copy when the work benefits from isolation; Git may expose merge conflicts. Pass `fork_turns: "none"` for every role and make `message` self-contained: goal, scope, paths, quick/medium/thorough boundary and stopping condition, constraints, expected output, absolute paths of user attachments and screenshots, the user's stated preferences and settled decisions, and what has already been checked or ruled out. Workers run checks for their changed scope, including rendered UI evidence when available. Reviewer checks the stated revision or diff and required behavior. The root spot-checks key and conflicting evidence plus integration and uncovered checks; do not repeat the whole investigation or every successful targeted check. Never put passwords, tokens or keys in `message`; keep work needing them in the root. Omit `fork_turns` only when the task depends on conversation content that cannot be restated as text or paths. Do not request interim progress: children message only for blockers or plan-changing facts, and final answers arrive through `agents.wait_agent`; use `agents.send_message` only for facts that change a child's task. When the gate confirms every active Explorer's current native turn is read-only, keep working on independent files, writes and commands included, and wait when you need its findings; tell an explorer if you edit files it is reading. Any other active child limits you to `agents.*` tools and the read tools `read_file`, `inspect_local_file`, `grep`, `glob`, and `view_image`, to prepare review, not redo delegated work. Stop waits for every attempt. After a terminal or fenced update, dispatch the next planned, unspawned task with a new `task_name` if a slot is free; otherwise return to `agents.wait_agent` with `timeout_ms: 120000`. Call unfiltered `agents.list_agents` only when a wait completes without per-agent terminal details or Codey's gate shows that an attempt left the active list. `MESSAGE` and mailbox updates are not completion. Treat `FINAL_ANSWER`, `task_complete`, `completed`, `errored`, `error`, `failed`, `shutdown`, and `not_found` as terminal. Use `agents.followup_task` to continue a child's own earlier work, even from a later turn: it keeps the child's context, so prefer it over a fresh spawn. Give one `codey_explorer` all related questions about an area and follow up with it on that area; spawn fresh only for unrelated work. If `CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not retry or wait for that target; take over or use a fresh `task_name`. A successful root interrupt permanently abandons and fences that attempt, revokes its tool access and settles it for the lifecycle ledger; do not wait for or follow up that target. Match evidence to the exact task and attempt; never reuse another attempt's no-change claim or infer a stall from elapsed time alone. If a child cannot decrypt its task body, restate the complete task with `agents.send_message` exactly once; do not interrupt or respawn it, and take over if that fails. On `CODEY_SUBAGENT_DUPLICATE_TASK_ID`, call unfiltered `agents.list_agents` once: wait for the original if present, otherwise take over; only a materially changed task may retry once with a fresh `task_name`. If every spawn fails, take over; if collaboration tools are unavailable, do not loop on an unregistered tool. Continue until all planned work has been performed and every attempt is terminal or fenced, then validate the combined result and the integration or uncovered checks required by project risk before continuing or finishing."#####;

pub(crate) fn root_agent_collaboration_usage_hint() -> String {
    ROOT_AGENT_COLLABORATION_USAGE_HINT.to_string()
}

const PREVIOUS_ROOT_AGENT_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools are direct \
commentary tools in native clients; never call them through `functions.exec` as a JavaScript \
aggregate. If a client explicitly exposes a tool catalog through a transport-only endpoint, use \
that endpoint with the exact catalog name and argument schema; a catalog-listed tool is available \
even without a same-named direct schema. Do not invent wrappers or bypass lifecycle and permission \
checks. Spawn one child per part that justifies one; never fill the two slots for their own \
sake. Assign distinct files to parallel writers and have them re-read a file before \
editing it; when two writers must change the same code, serialize them or give each an isolated \
worktree and let Git surface the conflict at merge time. Pass `fork_turns: \"none\"` for every \
role and make `message` self-contained: goal, scope, paths, constraints, expected output, absolute \
paths of user attachments and screenshots, the user's stated preferences and settled decisions, and \
what has already been checked or ruled out. Never put passwords, tokens or keys in `message`; \
keep work needing them in the root. Omit `fork_turns` only when the task depends on \
conversation content that cannot be restated as text or paths. Do not request interim progress: \
children message only for blockers or plan-changing facts, and final answers arrive through \
`agents.wait_agent`; use `agents.send_message` only for facts that change a child's task. While \
only `codey_explorer` children run, keep working, writes and commands included, and wait when \
you need their findings; tell an explorer if you edit files it is reading. Any other active child \
limits you to `agents.*` tools and the read tools `read_file`, \
`inspect_local_file`, `grep`, `glob`, and `view_image`, to prepare review, not redo delegated work. Stop waits for every \
attempt. After a terminal or fenced update, dispatch the next planned, unspawned task with a new \
`task_name` if a slot is free; otherwise return to `agents.wait_agent` with \
`timeout_ms: 120000`. Call unfiltered `agents.list_agents` only when a wait \
completes without per-agent terminal details or Codey's gate shows that an attempt left the active \
list. `MESSAGE` and mailbox updates are not completion. Treat `FINAL_ANSWER`, `task_complete`, \
`completed`, `errored`, `error`, `failed`, `shutdown`, and `not_found` as terminal. Use \
`agents.followup_task` to continue a child's own earlier work, even from a later turn: it keeps \
the child's context, so prefer it over a fresh spawn. Give one `codey_explorer` all related \
questions about an area, and follow up with it on that area; spawn fresh only for unrelated \
work. If \
`CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not retry or wait for that target; \
take over or use a fresh `task_name`. A successful root interrupt \
permanently abandons and fences that attempt, revokes its tool access and settles it for the \
lifecycle ledger; do not wait for or follow up that target. Match evidence to the exact task and attempt; never \
reuse another attempt's no-change claim or infer a stall from elapsed time alone. If a child \
cannot decrypt its task body, restate the complete task with `agents.send_message` exactly once; \
do not interrupt or respawn it, and take over if that fails. On \
`CODEY_SUBAGENT_DUPLICATE_TASK_ID`, call unfiltered `agents.list_agents` once: wait for the \
original if present, otherwise take over; only a materially changed task may retry once with a \
fresh `task_name`. If every spawn fails, take over; if collaboration tools are unavailable, do not \
loop on an unregistered tool. Continue until all planned work has been performed and every attempt \
is terminal or fenced, then validate the combined result, including the full tests and formatting \
that workers leave to the root, before continuing or finishing.";

const FIVE_ROLE_COLLABORATION_USAGE_HINT: &str = "\
`agents.spawn_agent`, `agents.wait_agent`, and other `agents.*` collaboration tools are direct \
commentary tools in native clients; never call them through `functions.exec` as a JavaScript \
aggregate. If a client explicitly exposes a tool catalog through a transport-only endpoint, use \
that endpoint with the exact catalog name and argument schema; a catalog-listed tool is available \
even without a same-named direct schema. Do not invent wrappers or bypass lifecycle and permission \
checks. Before waiting, dispatch independent tasks up to the concurrency limit of two; \
investigation, review and edits may share a workspace. Roles have the same tools; native sandbox, \
approval policy, permission profile and writable roots bound what each tool can touch. Assign \
distinct files to parallel writers and have them re-read a file before editing it; when two \
writers must change the same code, serialize them or give each an isolated worktree and let Git \
surface the conflict at merge time. Pass `fork_turns: \"none\"` for every role and make `message` \
self-contained: goal, scope, paths, constraints, expected output, absolute paths of user \
attachments and screenshots, the user's stated preferences and settled decisions, and what has \
already been checked or ruled out. Omit `fork_turns` only when the task depends on conversation \
content that cannot be restated as text or paths. Do not request interim progress: children \
message only for blockers or plan-changing facts, and final answers arrive through \
`agents.wait_agent`; use `agents.send_message` only for facts that change a child's task. While \
any attempt is active, Codey's gate allows only `agents.*` tools plus `read_file`, \
`inspect_local_file`, `grep`, `glob`, and `view_image`; use them between waits to prepare \
review, not to redo delegated work. Writes, commands and Stop wait until every attempt \
settles. After a terminal or successfully fenced update, check remaining capacity before \
dispatching the next planned, unspawned task with a new `task_name`; otherwise return to \
`agents.wait_agent` with `timeout_ms: 120000`. A timed-out wait only means no new message arrived; \
call `agents.wait_agent` again directly. Call unfiltered `agents.list_agents` only when a wait \
completes without per-agent terminal details or Codey's gate shows that an attempt left the active \
list. `MESSAGE` and mailbox updates are not completion. Treat `FINAL_ANSWER`, `task_complete`, \
`completed`, `errored`, `error`, `failed`, `shutdown`, and `not_found` as terminal. Use \
`followup_task` only for a bound nonterminal attempt. If \
`CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT` is denied, do not retry or wait for that target; \
take over or use a fresh `task_name` for a materially changed task. A successful root interrupt \
permanently abandons and fences that attempt, revokes its tool access and settles it for the \
lifecycle ledger; do not wait for or follow up that target. Queued followups may still start \
another turn but cannot restore tool access. Match evidence to the exact task and attempt; never \
reuse another attempt's no-change claim or infer a stall from elapsed time alone. If a child \
cannot decrypt its task body, restate the complete task with `agents.send_message` exactly once; \
do not interrupt or respawn it, and take over if that fails. On \
`CODEY_SUBAGENT_DUPLICATE_TASK_ID`, call unfiltered `agents.list_agents` once: wait for the \
original if present, otherwise take over; only a materially changed task may retry once with a \
fresh `task_name`. If every spawn fails, take over; if collaboration tools are unavailable, do not \
loop on an unregistered tool. Continue until all planned work has been performed and every attempt \
is terminal or fenced, then validate the combined result, including the full tests and formatting \
that workers leave to the root, before continuing or finishing.";

const EARLY_DELEGATION_MULTI_AGENT_MODE_HINT: &str = "Proactive multi-agent delegation is \
active. Any earlier instruction requiring an explicit user request before spawning sub-agents no \
longer applies. Proactively identify independent research, implementation, and verification tasks and \
delegate them early when parallel work, context isolation, or specialized evidence materially helps. \
Do not finish the same broad investigation in the root before delegating it. Keep one-step or inseparable \
sequential work with the root; file counts and tool-call estimates are not delegation cutoffs. When delegating, strongly \
prefer an enabled Codey role that fits the task and explicitly set `agent_type`: `codey_quick_scan` for \
focused lookups; `codey_deep_research` for broad code, log, and document research; \
`codey_visual_analysis` for visual evidence review and independent verification of frontend changes; \
`codey_visual_worker` for frontend and UI implementation; and `codey_worker` for backend, data, \
algorithm, CLI and script logic. A change that alters what a user can see belongs to \
`codey_visual_worker`, not `codey_worker`; routing it to the logic role drops render verification. Prefer these \
over generic `default`, `explorer`, or `worker` when both fit; avoid omitting `agent_type` out of habit. \
This is a preference, not a restriction: an explicit user choice, unavailable or unsuitable Codey roles, \
or a clear task-specific advantage can justify another available role. Roles select task specialization \
and models, with the same tool capabilities subject to native permissions and runtime availability. There is no fixed spawn \
budget: the concurrency limit is two; same-workspace investigation, review and edits of distinct files \
may run concurrently. Native sandbox, approval policy and writable roots bound each tool. \
Give parallel writers distinct files and re-read a file before editing it. When two writers must change \
the same code, serialize them or give each an isolated worktree and let Git surface the conflict at merge time. \
`CODEY_SUBAGENT_CONCURRENCY_LIMIT` means wait for a slot, not failure; when any child settles, \
check remaining capacity and fill a slot from the remaining planned independent work when allowed. If an active child \
cannot decrypt its task body, use `agents.send_message` exactly once to restate the complete task; do not \
interrupt or respawn it. If that fails, take over. After all attempts settle, validate their combined result \
before continuing or finishing. If every spawn fails, take over. On `CODEY_SUBAGENT_DUPLICATE_TASK_ID`, call \
unfiltered `agents.list_agents` once: wait for the original if present, otherwise take over. Only a \
materially changed task may retry once with a fresh `task_name`. This \
mode remains active until a later multi-agent mode developer message changes it.";

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

const PREVIOUS_ROOT_AGENT_MULTI_AGENT_MODE_HINT: &str = "\
Sub-agents never spawn and report only to the root. The root works by default; any earlier rule \
requiring an explicit user request before spawning is relaxed only as follows, and an explicit \
user request to delegate always applies. Keep in the root: a single search or read, Git status, \
diff and commits, release or version checks, a single host or service check, short questions, \
edits at a known location, and any step whose raw output you must read anyway. Send read-only \
research that takes several steps (search, read, then follow references, call paths, logs or \
documents) to `codey_explorer` at any size, stating quick, medium or thorough; Codey roles may run \
cheaper models. Send implementation of any kind, frontend or backend, to `codey_worker`, and \
independent verification of finished changes, including rendered UI, to `codey_reviewer`, only \
when that part is independent, clearly larger than a handful of tool calls, and returns a compact \
result you will not redo. Always set `agent_type`. At most two sub-agents run at once; \
`CODEY_SUBAGENT_CONCURRENCY_LIMIT` means wait for a slot. This mode remains active until a later \
multi-agent mode developer message changes it.";

const FIVE_ROLE_MULTI_AGENT_MODE_HINT: &str = "\
Proactive multi-agent delegation is active for the root agent; any earlier rule requiring an \
explicit user request before spawning no longer applies. Sub-agents never spawn and report only to \
the root. Do small, well-scoped work in the root: bug fixes, edits at a known location, short \
questions, one- or two-file changes that a few tool calls can make and verify, and inseparable \
sequential steps. Delegate multi-step work with independent parts, such as cross-directory \
research, multi-file or frontend-plus-backend changes, heavy log or document reading, or \
independent verification; after a few boundary checks, spawn instead of finishing that research in \
the root. For borderline tasks, delegate only when it clearly costs less than direct work, \
counting dispatch, waiting and review. Explicitly set `agent_type` to an enabled Codey role: \
`codey_quick_scan` for focused lookups, `codey_deep_research` for broad research, \
`codey_visual_analysis` for visual evidence and frontend verification, `codey_visual_worker` for \
any change users can see, `codey_worker` for backend, data, CLI and script logic. This is a \
preference, not a restriction: a user choice or no fitting Codey role justifies another available \
role. At most two sub-agents run at once; `CODEY_SUBAGENT_CONCURRENCY_LIMIT` means wait for a \
slot. This mode remains active until a later multi-agent mode developer message changes it.";

/// Detect Codey-owned mode hints written by earlier releases.
pub(crate) const ROOT_AGENT_MULTI_AGENT_MODE_HINT_VERSIONS: &[&str] = &[
    ROOT_AGENT_MULTI_AGENT_MODE_HINT,
    PREVIOUS_ROOT_AGENT_MULTI_AGENT_MODE_HINT,
    FIVE_ROLE_MULTI_AGENT_MODE_HINT,
    EARLY_DELEGATION_MULTI_AGENT_MODE_HINT,
];

/// Remove the previous owned paragraph when installing the current guidance.
pub(crate) const ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS: &[&str] = &[
    ROOT_AGENT_COLLABORATION_USAGE_HINT,
    PRE_SCOPED_COMPLETION_COLLABORATION_USAGE_HINT,
    PREVIOUS_ROOT_AGENT_COLLABORATION_USAGE_HINT,
    FIVE_ROLE_COLLABORATION_USAGE_HINT,
    SELECTIVE_FORK_COLLABORATION_USAGE_HINT,
    WAIT_120S_COLLABORATION_USAGE_HINT,
    WAIT_30S_COLLABORATION_USAGE_HINT,
    LIST_AFTER_TIMEOUT_COLLABORATION_USAGE_HINT,
    COMMAND_WORKSPACE_CLAIM_COLLABORATION_USAGE_HINT,
    WORKSPACE_WRITE_COLLABORATION_USAGE_HINT,
    WRITE_AT_SPAWN_COLLABORATION_USAGE_HINT,
    ROLE_BASED_COLLABORATION_USAGE_HINT,
    legacy::DIRECT_COLLABORATION_USAGE_HINT,
    PRE_INTERRUPT_FENCING_USAGE_HINT,
    legacy::BATCH_RESOLUTION_USAGE_HINT,
    legacy::DIRECT_SCHEMA_USAGE_HINT,
];

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
sandbox_mode = "read-only"

developer_instructions = """
你是检索子代理，只读：不修改、创建或删除文件，不做方案取舍，也不派生其他子代理。优先使用读取、搜索和查看现有图片的工具；只有当前轮原生只读沙箱已确认时，才可执行 `git status/diff/log`、日志查询等命令。通用执行包装、界面操作和未归类工具不可用，需要这些操作时交回主代理。
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
/// from these definitions on each launch, so no historical text has to be
/// recognized or matched. Only text injected into Codex's own `config.toml`
/// needs migration lists, because that file belongs to the user.
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

pub(crate) const CODEY_FASTCTX_GUIDANCE_VERSIONS: &[&str] = &[
    CODEY_FASTCTX_GUIDANCE,
    legacy::DIRECT_FASTCTX_GUIDANCE,
    legacy::TASK_ROUTED_FASTCTX_GUIDANCE,
];

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
    let collaboration_hint = root_agent_collaboration_usage_hint();
    let current_is_present = guidance_paragraph_start(existing, &collaboration_hint).is_some();
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
    updated.push_str(&collaboration_hint);
    updated
}

pub(crate) fn remove_subagent_guidance(current: &str) -> Option<String> {
    let mut restored = current.to_string();
    let mut changed = false;
    for &guidance in SUBAGENT_GUIDANCE_VERSIONS {
        while let Some(without_guidance) = remove_owned_guidance_block(&restored, guidance)
            .or_else(|| remove_owned_guidance_paragraph(&restored, guidance.trim()))
        {
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
    fn historical_collaboration_hints_are_removed_without_dropping_user_text() {
        let custom = "Preserve my collaboration policy.";
        for previous in ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS {
            let input = format!("{custom}\n\n{previous}");
            let output = append_root_agent_collaboration_usage_hint(&input);
            assert_eq!(
                output,
                format!("{custom}\n\n{}", root_agent_collaboration_usage_hint())
            );
            assert!(!output.contains("CODEY_DELEGATION_V2"));
            assert!(!output.contains("resolve_batch"));
            assert_eq!(append_root_agent_collaboration_usage_hint(&output), output);
        }
        let customized = format!(
            "{} Additional user restriction.",
            legacy::DIRECT_SCHEMA_USAGE_HINT
        );
        assert!(append_root_agent_collaboration_usage_hint(&customized).starts_with(&customized));
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
        assert!(combined.contains("Explorer's native sandbox is read-only"));
        assert!(combined.contains("Reviewer uses workspace-write"));
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
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&format!(
                "{custom}\n\n{PRE_INTERRUPT_FENCING_USAGE_HINT}"
            )),
            combined
        );
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&format!(
                "{custom}\n\n{ROLE_BASED_COLLABORATION_USAGE_HINT}"
            )),
            combined
        );
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&format!(
                "{custom}\n\n{WRITE_AT_SPAWN_COLLABORATION_USAGE_HINT}"
            )),
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
    fn historical_subagent_guidance_cleanup_handles_serialized_trailing_newlines() {
        for previous in SUBAGENT_GUIDANCE_VERSIONS {
            for body in [*previous, previous.trim()] {
                assert_eq!(
                    remove_subagent_guidance(&format!("USER\n\n{body}")),
                    Some("USER".to_string())
                );
            }
        }
        let customized = format!(
            "{} Additional user restriction.",
            legacy::DIRECT_SUBAGENT_GUIDANCE.trim()
        );
        assert_eq!(remove_subagent_guidance(&customized), None);
    }

    #[test]
    fn multi_agent_mode_hint_uses_uniform_concurrency_without_spawn_budgets() {
        for role in crate::config::SUBAGENT_ROLE_IDS
            .into_iter()
            .filter(|role| *role != crate::config::SUBAGENT_ROLE_DEFAULT)
        {
            assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains(role), "{role}");
        }
        for (retired, _) in crate::config::LEGACY_SUBAGENT_ROLES {
            assert!(
                !ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains(retired),
                "{retired}"
            );
            assert!(
                !ROOT_AGENT_COLLABORATION_USAGE_HINT.contains(retired),
                "{retired}"
            );
        }
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("Always set `agent_type`"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("At most two sub-agents run at once"));
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("Sub-agents never spawn"));
        for retired in [
            "Known file edits claim their complete target paths",
            "opaque operations retain workspace scope",
            "unresolved targets require conservative exclusion",
        ] {
            assert!(
                !ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains(retired),
                "{retired}"
            );
        }
        assert!(ROOT_AGENT_MULTI_AGENT_MODE_HINT.contains("`CODEY_SUBAGENT_CONCURRENCY_LIMIT`"));
        // Recovery rules live in the root-only usage hint; the mode hint also
        // reaches every sub-agent and is truncated past its byte budget.
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
        // Codex keeps only the first and last 800 bytes of a longer
        // `multi_agent_mode_hint_text`, silently dropping the middle.
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
        assert_eq!(
            append_root_agent_collaboration_usage_hint(&format!(
                "Keep this.\n\n{FIVE_ROLE_COLLABORATION_USAGE_HINT}"
            ))
            .matches(&root_agent_collaboration_usage_hint())
            .count(),
            1
        );
        assert_eq!(
            ROOT_AGENT_MULTI_AGENT_MODE_HINT_VERSIONS[0],
            ROOT_AGENT_MULTI_AGENT_MODE_HINT
        );
        for previous in [
            CONSERVATIVE_SUBAGENT_GUIDANCE,
            EARLY_DELEGATION_SUBAGENT_GUIDANCE,
        ] {
            assert_eq!(
                remove_subagent_guidance(&format!("USER\n\n{previous}")),
                Some("USER".into())
            );
        }
        for retired in SUBAGENT_GUIDANCE_VERSIONS {
            assert_eq!(
                remove_subagent_guidance(&format!("USER\n\n{retired}")),
                Some("USER".into())
            );
        }
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
    fn root_hint_replaces_the_list_after_timeout_rule() {
        // The gate's timeout feedback tells the root to wait again directly.
        assert!(!ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("If a wait times"));
        let custom = "Preserve my root usage hint.";
        let upgraded = append_root_agent_collaboration_usage_hint(&format!(
            "{custom}\n\n{LIST_AFTER_TIMEOUT_COLLABORATION_USAGE_HINT}"
        ));
        assert!(upgraded.contains(custom));
        assert!(!upgraded.contains("If a wait times"));
        assert_eq!(
            upgraded
                .matches(&root_agent_collaboration_usage_hint())
                .count(),
            1
        );
    }

    #[test]
    fn every_role_starts_without_parent_history_by_default() {
        assert!(
            ROOT_AGENT_COLLABORATION_USAGE_HINT.contains(r#"`fork_turns: "none"` for every role"#)
        );
        let upgraded = append_root_agent_collaboration_usage_hint(&format!(
            "Keep this.\n\n{WAIT_30S_COLLABORATION_USAGE_HINT}"
        ));
        assert!(upgraded.contains("Keep this.") && !upgraded.contains("`timeout_ms: 30000`"));
        assert_eq!(
            upgraded
                .matches(&root_agent_collaboration_usage_hint())
                .count(),
            1
        );
        let restored =
            remove_subagent_guidance(&format!("Keep this.\n\n{INHERITED_FORK_SUBAGENT_GUIDANCE}"))
                .unwrap();
        assert_eq!(restored.trim(), "Keep this.");
    }

    #[test]
    fn unforked_retrieval_tasks_carry_attachments_and_settled_context() {
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("absolute paths of user attachments"));
        assert!(
            ROOT_AGENT_COLLABORATION_USAGE_HINT.contains("cannot be restated as text or paths")
        );
        let upgraded = append_root_agent_collaboration_usage_hint(&format!(
            "Keep this.\n\n{WAIT_120S_COLLABORATION_USAGE_HINT}"
        ));
        assert!(upgraded.contains("Keep this."));
        assert_eq!(
            upgraded
                .matches(&root_agent_collaboration_usage_hint())
                .count(),
            1
        );
        let restored = remove_subagent_guidance(&format!(
            "Keep this.\n\n{RETRIEVAL_NO_FORK_SUBAGENT_GUIDANCE}"
        ))
        .unwrap();
        assert_eq!(restored.trim(), "Keep this.");
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
        assert!(EXPLORER_AGENT_CONFIG.contains("只有当前轮原生只读沙箱已确认时"));
        assert!(EXPLORER_AGENT_CONFIG.contains("通用执行包装、界面操作和未归类工具不可用"));
        assert!(WORKER_AGENT_CONFIG.contains("前端界面与交互、后端"));
        assert!(
            WORKER_AGENT_CONFIG
                .contains("改动影响用户可见界面时，优先用客户端已授权的渲染或截图工具核验")
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
        for (retired, _) in crate::config::LEGACY_SUBAGENT_ROLES {
            assert!(subagent_source_config(retired).is_none(), "{retired}");
        }
    }

    #[test]
    fn root_hint_lists_the_exact_active_gate_read_allowlist() {
        let expected = "`read_file`, \
`inspect_local_file`, `grep`, `glob`, and `view_image`";
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains(expected));
        assert!(ROOT_AGENT_COLLABORATION_USAGE_HINT.contains(
            "When the gate confirms every active Explorer's current native turn is read-only"
        ));
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
    fn owned_guidance_versions_are_unique_and_resolvable() {
        for versions in [
            SUBAGENT_GUIDANCE_VERSIONS,
            ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS,
        ] {
            let mut seen = std::collections::BTreeSet::new();
            for version in versions {
                let trimmed = version.trim();
                assert!(!trimmed.is_empty());
                assert!(seen.insert(trimmed), "duplicate owned guidance version");
            }
        }
        assert_eq!(
            ROOT_AGENT_COLLABORATION_USAGE_HINT_VERSIONS[0].trim(),
            ROOT_AGENT_COLLABORATION_USAGE_HINT.trim()
        );
    }

    #[test]
    fn role_sandboxes_match_their_actual_capabilities() {
        for role in crate::config::SUBAGENT_ROLE_IDS {
            let source = subagent_source_config(role).unwrap();
            assert!(source.contains(&format!("name = \"{role}\"")));
            assert!(source.contains("description = \""));
        }
        assert!(EXPLORER_AGENT_CONFIG.contains("sandbox_mode = \"read-only\""));
        assert!(REVIEWER_AGENT_CONFIG.contains("sandbox_mode = \"workspace-write\""));
        assert!(WORKER_AGENT_CONFIG.contains("sandbox_mode = \"workspace-write\""));
        assert!(DEFAULT_AGENT_CONFIG.contains("sandbox_mode = \"workspace-write\""));
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
