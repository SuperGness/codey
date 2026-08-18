# Codey 内部开发文档

本文档面向 Codey 的开发和维护，保留实现细节、构建发布流程、配置路径、启动恢复机制和已知限制。面向使用者的功能介绍只维护在 `README.md`；不要把协议、端口、路径、构建命令、数据库结构、补丁策略或其他内部技术细节迁回公开 README。

Codey 是一个无界面的 Rust 桌面辅助进程，通过 CDP 连接官方 Codex Electron 客户端，并把 React 配置控制台直接注入 Codex 页面内的隔离浮层。官方线路仍由 Codex 直接连接；OpenAI 兼容的 Chat Completions 第三方线路，以及模型目录中包含第三方模型的原生 Responses 第三方线路，会在 Codex 运行期间启用仅绑定回环地址、使用随机端口的临时协议代理。第三方运行时 provider 配置和代理生命周期均由 Codey 管理；CC Switch 非 Live 线路在首次 Renderer 就绪后会先从磁盘恢复其原 provider 表，其他运行时覆盖仍由租约在退出时原子恢复，代理则跟随受控 Codex 进程关闭。

## 当前能力

- 原生任务 hydration 的 stream owner 发现按 renderer 内的 `clientCoordination` 实例隔离：同一 `hostId + conversationId` 的并发查询复用一份 in-flight Promise，查询完成后立即移除，不缓存成功 owner。后续 hydration 每次重新确认当前仍存活的 owner，避免已断开的旧 owner 让 renderer 误进入 follower 状态、跳过本地历史补载并忽略后续增量消息。空结果、异常和 150 毫秒超时同样不会保留，下一次仍会重新发现；协调器替换、renderer 重载或路由重启会随 WeakMap / 页面生命周期整体失效。
- 启动器的 `CodeyRuntime::start()` 只负责编排七个有序阶段：诊断存储保护、线路快照解析、启动前存储维护、运行时 Provider 配置、补丁与路由监听、进程启动及首屏注入、运行期 watcher 安装；阶段顺序、错误记录、失败恢复和 receiver 返回语义保持不变。macOS / Windows 的 Electron 启动补丁源码独立维护在 `backend/src/codex_startup_patch.js`，Rust 通过 `include_str!` 编译进二进制，前端检查会先执行 Node 语法校验。共享 bridge 统一提供 Statsig 客户端发现、React 内部键枚举以及可配置祖先深度的 fiber 图检索；模型白名单与宠物盾牌不再各自实现 React host 扫描。模型配置 hook 在源头用 `useCallback` 发布业务回调，根 `App` 直接把这些回调传入 memo 子组件，不再为同一组回调逐个建立 ref、layout effect 和外层 callback。
- 打开 Codey 时自动启动 Codex，并通过 CDP 注入 Codey 设置按钮、Fast 模式展示修复、插件市场修复和消息选择工具；设置按钮在 Codex 客户端内部打开 Shadow DOM 隔离的 Semi Modal 配置浮层，不跳转外部浏览器。
- 配置页运行状态卡通过 `runtime_status` 展示 Codey 版本、Codex App 路径、Codex App 版本和维护状态；`codexAppVersion` 优先读取当前受控 runtime 的应用目录，其次读取用户保存的应用路径，不在普通状态轮询里做全系统发现。
- Windows 原生 EXE 使用 GUI 子系统，运行期间不会创建命令行窗口。首次启动 Codex 遇到普通不可恢复错误时，Codey 会恢复临时配置、显示系统错误对话框并退出；CC Switch 路由尚未稳定时则保持后台进程存活，每秒只读复核路由，连续两次得到完整有效快照后自动重试启动，避免外部启动项形成退出与拉起循环。清理失败时，对话框和诊断日志会同时保留启动错误与清理错误。
- 线路同步始终直接读取 Codex `config.toml` 中的活动 provider，并从 provider token 或 `auth.json` API Key 取得凭据，不再根据 CC Switch 数据库选择地址、凭据或活动线路。唯一例外是已同时确认管理态与 Live 标记的 CC Switch 路由：模型列表请求会只读解析数据库中的当前源 API 地址和凭据，构造不持久化的临时请求配置，线路运行配置仍以 Codex 为准。该路径兼容用户手工维护以及 CC Switch 已写入 Codex 的第三方地址、`env_key`、`http_headers` 与 `env_http_headers`；请求扩展只保留在后端临时对象中，不进入 Codey 配置存储或 renderer。provider 范围内的 `experimental_bearer_token` 优先于 `auth.json` 中同时保留的 ChatGPT OAuth，不得把明确的第三方地址误判为官方线路。HTTP 客户端同时加载内置公共根证书与系统原生根证书，使 Windows 已信任的内网 CA 可用于第三方模型同步。CC Switch 路由关闭时，其部分 OpenAI Chat 线路为了满足 Codex 配置约束仍会写成 `wire_api = "responses"`；Codey 只在 Codex 当前直连 `base_url` 与 CC Switch 数据库当前线路的配置地址或 `provider_endpoints` 精确匹配时，读取不含凭据的 `meta.apiFormat` 作为协议提示。`openai_chat` 启用 Codey 协议代理，`openai_responses` 保持原生直连；数据库缺失、旧 schema、解析失败、手工地址不匹配或活动地址为回环代理时均忽略该提示。若活动 provider 以精确的 `name = "OpenAI"` 声明支持 Codex 远程压缩，Codey 会把该能力随线路配置持久化并在临时 provider 中保留。线路变化需要重启由 Codey 启动的 Codex 后生效。
- 官方线路沿用 ChatGPT 登录；原生 Responses 第三方线路继续把 API 地址和临时 bearer token 直接交给 Codex。OpenAI 兼容的 Chat Completions 线路启动时会生成一份不依赖全局状态的显式配置快照，把 Codex 的 Responses 请求经临时回环代理转换为 Chat Completions 请求，并把普通响应、SSE 文本流、工具调用、用量和错误转换回 Codex 所需格式。代理使用系统分配的临时端口并跟随受控 Codex 进程启停；本地 listener 最多同时保留 64 个连接，请求头与请求体分别限制为 64 KiB 和 32 MiB，请求读取使用 15 秒 idle timeout 与 45 秒总 deadline，长时 SSE 响应本身不施加总时限。上游请求复用同一个带连接池的 HTTP client，并单独施加 5 秒连接超时和普通/流式响应头超时；启动或运行时配置失败会立即回收。原生 Anthropic、Gemini 等非 OpenAI 兼容协议仍不在该适配范围内。携带第三方模型的原生 Responses 线路同样经该代理：代理按请求中的 `model` 逐请求选路，命中模型目录第三方模型集合时转换为 Chat Completions，其余官方模型原样直通上游 `/v1/responses`，因此同一会话内主模型与子代理模型可以使用不同协议；模型目录无第三方模型时不启动代理，保持原生直连。
- 官方账号线路默认开启浮动额度展示。额度组件以固定定位浮窗挂在 Codex 右下角，默认保留 24px 边距，套餐、周期额度、余额和本地刷新时间纵向展示；拖拽结束后把浮窗 `left/top` 保存在 Codex renderer 的 localStorage 中，并在窗口尺寸变化时约束回可视范围。轻量 renderer 每 60 秒通过 Codey bridge 请求一次额度快照；Rust 后端只在当前 provider 判定为官方且 `showAccountUsageInHeader` 已开启时读取 `auth.json` 的 ChatGPT access token 和 account ID，请求 ChatGPT backend 的 `/wham/usage`，并兼容 `/api/codex/usage` 旧路径。渲染层只接收已归一化的周期、使用比例、重置时间、方案和余额，不接收 OAuth 凭据；第三方线路、关闭开关或请求失败时会自动隐藏组件或保留上一次成功结果并标记为过期。
- CC Switch 路由接管通过数据库 `proxy_config.enabled`、Codex 的 `proxy_live_backup` 及旧版 `proxy_takeover_codex` 设置识别管理态，并用活动 provider 的 `PROXY_MANAGED` 标记或 `cc-switch-official` 回环地址验证 Live 接管态。启动时从同一次 `config.toml` / `auth.json` 读取中建立 Live 路由快照，活动 provider 必须存在对应表并带有效 HTTP(S) 地址；第三方线路不得使用 Codex 保留 provider ID。管理态存在但 Live 标记缺失、provider 悬空或地址无效时，Codey 在会话同步和代理启动前停止，提示用户关闭后重新开启路由。有效接管下沿用快照中的 provider、地址、凭据和协议，跳过 Codey 模型目录刷新，并且不再把推理档位、FastCtx、子代理或 Hook 状态写入 `config.toml`：这些 Codey-owned 字段由 Electron 启动补丁作为 app-server 命令级 `-c` 覆盖注入，优先级高于用户层而不污染 CC Switch 管理的文件。可直接编辑的约束源保存在 Codey 配置目录的 `codex-constraints/`：根代理规则为 `root-instructions.md`，FastCtx 规则为 `fastctx-instructions.md`，协作提示为 `collaboration-hint.md`，通用兜底子代理为 `subagent.toml`，另外五类任务的源配置位于 `agents/*.toml`。启动时根据设置页选择覆盖每份源配置的 `model` 与 `model_reasoning_effort`，并生成 `runtime/default-agent.toml` 和 `runtime/agents/*.toml`；源文件可编辑，运行副本不得直接编辑。六类角色分别通过 `agents.<role>.config_file` 引用运行副本，修改约束源或模型选择后需要受控重启 Codex 才会重新合成。旧版未编辑的根规则会按完整默认文本精确迁移，用户自定义内容不会被模糊检索替换。Hook 定义单独合并到稳定路径 `~/.codex/hooks.json`，只追加带 `--codey-subagent-gate-hook` 标记的 group；精确 `hooks.state` 信任哈希作为进程覆盖项注入，退出时按租约恢复或仅移除 Codey group。这样 CC Switch 切线可以整份重写 `config.toml` 而不覆盖 Codey 约束。活动地址为 CC Switch 回环代理时不会应用数据库中的 Chat 格式提示或启动第二个 Codey 协议代理。路由关闭且直连线路匹配 `openai_chat` 提示时，Codey 才把临时 provider 的 `base_url` 与 `wire_api` 覆盖为自己的 Responses 协议代理，同时保留真实上游地址和凭据用于 Chat Completions 转换；首次 Renderer 与 bridge 就绪后立即按 original/applied/current 三方合并恢复磁盘上的 `model_providers`，租约与代理继续存活，因此 cc-switch 后续切线不会把 Codey 的回环地址保存回旧线路。恢复前会复核活动 provider 与已应用端点，并在原子写前再次做字节 CAS；并发切线时保守跳过，不覆盖外部新配置。Live 接管必须持续保留 watcher，线路语义变化后通过受控重启重建进程覆盖项。
- 配置页以官方账号可见的 7 个模型为固定左列；每次拉起第三方线路前会在 5 秒上限内请求 `/v1/models` 或 `/models`，非路由模式使用 Codex 当前 provider，CC Switch 有效接管时则绕过回环代理、只读解析其当前源 API 地址和真实凭据。源地址必须是非回环 HTTP(S) 地址，`PROXY_MANAGED` 只能作为接管标记，绝不作为 bearer token 发往源服务；解析失败按普通模型同步失败处理。同步成功后仅向 Codex 展示上游支持的模型，无需再手动同步并重启。请求失败、超时或返回空列表时优先沿用该线路上次保存的模型支持配置，首次使用且尚无保存配置时才回退到固定 7 模型并继续启动。配置页手动同步失败后仍会打开模型弹框，明确提示线路可能不支持模型目录接口；弹框始终列出 7 个官方模型供用户勾选，并允许输入其他模型 ID。其他模型输入会在前后端同时拒绝官方清单中的模型，保存时把官方勾选与其他模型共同写为该线路已确认的支持范围。模型支持范围、上游目录或默认模型保存后，后端会通过当前 renderer 的 CDP 连接把新目录直接传给模型白名单 `setCatalog()`，避免保存请求内部再次调用 bridge 形成重入等待；renderer 同时改写 Statsig 模型配置、触发 `values_updated`、刷新 React Query 的 `models/list` 活跃缓存，并在 app-server 返回旧目录时于消息捕获阶段替换模型描述。模型白名单还会在 `thread/start`、`thread/resume`、`turn/start` 以及宿主的直接和包装 IPC 请求发出前，把缺失或已经不属于当前目录的模型替换为当前默认模型，避免线路切换后继续发送旧 GPT 模型。后端除校验 `snapshot()` 的模型顺序与默认模型外，还要求命中 Statsig 订阅和当前模型查询缓存才把本次保存报告为立即生效；运行时模型基线仅在这些校验成功后更新，因此模型变更可单独清除重启标记，热刷新失败时则保留重启要求。
- 启动前备份 Codex `config.toml`，退出时按 lease marker 原子恢复，`auth.json` 和官方登录状态保持不变。租约同时记录本次 Chat Completions 协议代理地址。应用临时配置时以启动路由快照做 CAS 校验，并在真正拉起 Codex 前再次核对 `config.toml` 与 `auth.json`，防止启动准备期间发生切线。非 Live 的 CC Switch Chat 线路会在首屏注入成功后先恢复磁盘 provider 表，但保留 applied snapshot、marker、FastCtx、模型目录、推理档位和子代理覆盖；停止或异常恢复仍用完整三方合并撤销剩余 Codey-owned 字段。CC Switch 路由模式从租约应用完成后每秒检查一次 Live 配置与认证；watcher 以活动 provider、去尾斜杠后的端点、缺省为 Responses 的 wire API、有效 provider 凭据及认证路由字段组成语义指纹，忽略 TOML 排版、字段顺序、默认字段补写、JSON 排版及 ChatGPT 账号 token 刷新；无法解析时保守回退原始字节比较。语义变化或配置连续两个检查周期缺失后不再把新 provider 指回旧协议代理，而是触发一次受控 Codex 重启；同一语义的新路由即使每次序列化字节不同，也用语义指纹完成稳定性去抖。若新快照仍处于管理态与 Live 文件不一致、文件写入中或启动前再次切线，Codey 不请求自身退出，而是保留 `startupError` 和重启任务、等待连续两个有效快照后再次拉起 Codex；普通启动故障仍沿用原退出策略。停止流程先结束 watcher 和旧 Codex，再关闭旧协议代理并按三方合并恢复最新 CC Switch 基线；新启动重新读取完整路由快照、同步会话并按需建立新协议代理，因此 provider、端点、token 和 wire API 不会跨快照混用。
- 启动器对 `sessions` 与 `archived_sessions` 的 rollout 采用逐行流式检查；只有确实需要改写 provider 的文件才会载入全文，避免长会话历史在启动时形成多份大字符串并把内存峰值长期留在分配器中。
- 启动器只读取 rollout 的首个 `session_meta` 头并流式遍历目录，不再为校验构建全量路径列表；头部校验按目录分片到最多 4 条线程并发执行，任一目录发现 provider 不匹配即整体提前结束。Trace 防护、Crashpad 容量收敛、插件维护和宠物状态会在依赖关系允许时并行执行；诊断存储统计只在用户请求时执行。Provider 迁移、陈旧锁恢复、应用目录解析、模型目录读写、所有 Codey 配置落盘、运行时 TOML 应用以及启动前、失败回滚和停止阶段的配置恢复都通过 blocking worker 执行，避免计划重启、失败清理或保存设置时阻塞仍存活的 async bridge；周期 watcher 写错误日志时使用可等待的 blocking 包装，退出与启动关键路径仍保留同步写入以保证落盘语义。恢复任务仍按原顺序等待完成，不会与进程回收或协议代理关闭并发。启动流程把初始 Renderer 注入、失败清理、watchdog 创建及跨平台进程停止收敛为独立 helper，保持原有失败恢复和 watcher 关闭顺序。配置写锁继续覆盖 CAS 校验、外部副作用、持久化和内存发布，以维持 revision 及磁盘/内存一致性。应用目录解析完成后，启动器先停止并等待旧 Codex 进程退出，再执行 rollout/provider 同步与会话索引清理，避免永久维护和仍在写入的 Codex 竞争；模型目录准备随后在 blocking worker 中执行。官方模型目录在同一次启动内按文件大小和修改时间复用解析结果，不再为 `refresh_for_provider` 和 `selection_state` 各解析一遍。
- Codey 的受控基础脚本会预构建为单个 CDP 文档注入包并在健康恢复时复用，默认注入从 16 次脚本往返降为 2 次；共享 bridge 必须先于慢启动保护执行，并统一提供 Statsig 客户端发现、React fiber 遍历和带优先级的单点 fetch 拦截注册。fast 与插件市场脚本不再层叠覆盖 `window.fetch`，重复注入只替换同名拦截器，最后一个拦截器撤销后恢复原生 fetch。约 689 KB 的 React 设置浮层、按需组件样式与主题变量只在首次点击 Codey 按钮时注入，用户脚本仍保持独立且最后执行。`public/` 注入脚本在 `vite:build` 阶段压缩到 `dist-overlay/inject/` 后才嵌入二进制；布尔占位符通过数组取值阻断 esbuild 的解析期常量折叠，构建脚本仍逐文件校验占位符幸存并在异常时回退源码，测试同时锁定占位符和压缩收益。浮层 CSS 会剔除所有逗号选择器都带 `-rtl` 类的独立规则，与 `body`/`:host` 共享选择器列表的主题变量块保持原样；本地 Badge 不再静态引入 Semi Tag 及其 Avatar 样式，Card、Modal 的传递依赖在没有产物级视觉白名单前不做选择器盲删。额度组件在数值未变化时跳过 DOM 重建；CDP 注入重试采用约 30 秒总预算内的指数退避，为新版 Windows Codex 较慢的 Renderer 资产准备保留实际注入时间；每 60 秒的额度刷新会记住上次成功的接口端点，失败时仍回退完整列表。
- 配置页的通知、确认框和 Codex 路径弹窗各自使用独立的外部 store；只有对应的 memo host 通过 `useSyncExternalStore` 订阅，提示文本、确认内容和路径输入变化不会重新执行根 `App`。根组件只保留跨面板的配置、运行状态、busy、portal 与诊断快照状态。运行状态响应在写入前按值复用未变化的维护、注入与诊断子快照，完全相同的轮询结果不会提交 React 更新；更新卡、功能策略和运行面板只接收各自需要的稳定切片，诊断、重启和注入复核共用一个有界调度器与单飞状态请求，不会再穿透这些 memo 边界。需要刷新注入证据时由同一次 `runtime_status` 请求完成，Codex App 版本探测按运行目录和配置目录缓存 30 秒。
- 后端核心入口保持为薄门面：Codex 配置中的 FastCtx TOML 与旧租约恢复、命令层的诊断存储与插件市场、启动器的 CC Switch 路由监听与跨平台进程生命周期分别维护在独立子模块中；大型 Rust 单元测试模块也与生产入口文件分开存放，但仍作为对应父模块的子模块访问私有实现。前端设置浮层壳、稳定事件 hook、模型分页策略和各功能域样式同样独立维护；嵌入浮层按固定顺序拼接样式片段，开发预览按同一顺序加载。测试优先直接执行 Rust 逻辑或可独立运行的 TypeScript 策略，源码扫描只保留跨构建边界、注入接线和发布内容等无法低成本行为化的契约门禁。
- `codey-errors.log` 继续只记录失败，并保持逐行 JSON。每条记录只保留北京时间（秒精度）、平台、可取得的 Codey/Codex/Electron/Chrome/Node 版本、事件、操作、错误文本及可选的阶段、可恢复标记和故障所需最小上下文；不再写入毫秒时间戳、PID、耗时、重试次数或超时副本。旧版主进程补丁 helper 记录仍可兼容读取，UTC 或本地时间会统一换算到 `+08:00`，旧 `context` 中的运行时版本会迁入 `versions`。Codey 主进程和内嵌 FastCtx sidecar 都在顶层错误与 Rust panic 时同步写入该日志；FastCtx 额外区分 MCP transport 关闭与普通运行失败，并标注 MCP、runtime-bootstrap、runtime-host 或 CLI 阶段。`SIGKILL`、OOM 强杀、断电等无法执行进程内 hook 的终止仍不会产生子进程自记录，需要结合 Codex/MCP 宿主或系统日志判断。协议代理不得持久化 bearer token、API Key、请求正文或用户提示词。CDP 注入仍使用约 30 秒硬 deadline，但详细耗时与重试信息只留在运行态诊断，不进入错误日志。
- 共享 app state 默认仍位于用户目录下的 `.codex-session-delete`；需要跨进程隔离状态的测试或本地调试可设置 `CODEY_APP_STATE_DIR` 指向完整 state 目录，空值会被忽略并回退默认路径。
- Renderer 启动时只保留设置按钮和三个带侧边栏目标过滤的轻量交互监听；监听在 React 挂载侧边栏前就绪，导入、导出、删除、相对时间和消息选择等会话工具仍要等用户首次悬停、点击或键盘聚焦侧边栏后才加载，加载完成后会撤掉这些监听和启动观察器。增量观察器按新增控件最近的会话行、项目行、侧边栏分区或消息轮次修复，刷新前再次合并祖先/后代根节点，且仅在顶栏确实变化时重找设置按钮；节流在持续变更下最多推迟 250 毫秒，避免流式输出把刷新无限期饿死。侧边栏属性与子节点 mutation 只把受影响的会话行加入同一合并队列，不在每条 observer 记录内同步遍历 React fiber 或递归检查状态栏；Codey 只给官方确认仍在运行的任务外层写入稳定的 running 标记，并通过 flex `order` 在各自列表内建立单一运行中分桶，不移动 React DOM，也不改变多个运行中任务或多个非运行任务之间的官方相对顺序。原生运行状态短暂消失时按会话保留 2 秒 running 标记，并监听 `aria-hidden` 与 `hidden` 变化后复核，避免 React 状态栏切换节点时任务瞬间掉回普通分桶；状态持续缺失才刷新完成时间并释放标记。项目首次展开时，如果 React 完整会话键表明已知运行任务仍属于该项目、但首批 DOM 行尚未包含它，Codey 只触发该项目原生的“展开显示”，新增行仍沿观察器路径标记并置顶；没有隐藏运行任务的项目不展开。置顶、等待介入、未读、最近更新和手动排序仍由官方列表负责。命中带 `data-turn-key` 的消息轮次根节点时直接复用该根，不再枚举整轮后代；消息路径也只跑选择安装器，不执行侧边栏安装器。会话 ID 探测只在用户真正硬删过消息后才进行，消息选择按钮按行缓存而非每次全子树查找。相对时间只遍历已登记且仍连接的会话行并跳过无变化的 DOM 写入；本地任务在首次挂载、任务完成、窗口回前台及页面可见的一分钟节拍通过官方 app-server 更新内存时间缓存，窗口回前台的强制刷新按 10 秒去抖，普通扫描对同一会话按 60 秒限流；远程任务直接复用官方 React 行已持有的 `updated_at` / `created_at`，不误发本地 thread 请求。观察器额外跟踪项目展开状态与原生“全部显示”状态，不监听流式正文的 `characterData` 或无业务消费者的 `style` 变更；`class` 仍用于识别原生会话运行态与 spinner，不得移除。插件 bridge 使用有界指数退避等待宿主接口，也不会再序列化无关 IPC 的完整参数，并在解析请求体前先做子串预筛，避免为无关请求整体 `JSON.parse`。
- 宠物屏蔽脚本不会跨扫描缓存 React fiber 判定：React 可能复用 host element 并独立替换 props/fiber；性能由 bridge 的单个 document-root `MutationObserver`、合并后的 `attributeFilter`、有界根队列和帧调度控制。宠物与完全访问权限提示共用该观察器，最后一个订阅撤销时才断开；宠物脚本还复用 bridge 提供的控件描述归一化、控件子树查询、事件拦截与 teardown 骨架。renderer 启动观察器会在会话工具接管后断开，正式会话工具观察器仍按生命周期接棒，不并入盾牌分发器。完全访问权限提示只扫描新插入的子树并改用 `textContent`，不再每次触发整页按钮遍历和布局刷新。模型白名单的交互重扫按 2 秒节流，未找到 QueryClient 时的完整 React 图发现最多每 10 秒执行一次；目录加载和已加载目录的短时安全重投递都按 120 毫秒起步指数退避，后者上限 1 秒且不会并发执行两次投递，前者上限 2 秒且同一时刻只保留一个刷新计时器；相同目录的后台重推和窗口聚焦重载都会跳过全量失效投递。原生任务 hydration 仍先尝试发现其他窗口的现有 stream owner，但本地协调超过 150 毫秒即继续 `thread/read`/`thread/resume`，不再等待上游固定 5 秒超时；慢启动保护的 Statsig 客户端轮询在首秒后从 50 毫秒退避到 250 毫秒。
- 后台会话状态轮询对每个变更的 rollout 采用可续解析：JSONL 只追加时按已消费字节偏移续读并只解析新增行，因此活跃会话不再每 3 秒重读整份历史；首次读取、重写后的全量回退和增量尾部都通过复用行缓冲区流式消费，不再把整份 rollout 读成一个大字符串。缓存只保留一份可续解析 state；文件变化时直接接管旧 state 的所有权，最终聚合时才生成调用方需要的拥有型结果。无 rollout 变化且没有待确认调用时，缓存与 watcher 通过同一个只读 `Arc` 复用上一轮聚合快照；存在待确认时只重建持续时间会变化的 pending 列表，started/aborted/completed 事件、session 状态与 turn 配置继续按各自 `Arc` 复用，不再每轮深复制 5 个 `Vec` 和 1 个 `HashMap`。每个 rollout 只保留最近 256 个终态 turn 及最多 512 份 turn 配置，终态到达时同步清除该 turn 的待确认调用；通知 tracker 的终态去重集合上限与 64 个最近会话的缓存总容量一致，避免长会话轮询导致 Codey 常驻内存与每轮复制成本持续增长。已消费前缀的头尾各 64 字节使用固定内联缓冲区保存并在续读前校验，校验读取不再临时分配 `Vec`；Codey 自身重写 rollout（删除对话轮、归一 provider）或文件被截断时自动回退为全量解析。只读 SQLite 连接会在数据库文件未变化时跨轮询复用，避免稳定空闲期反复打开同一状态库。会话标题缓存的同步锁与 SQLite 工作整体位于 blocking worker 内，async future 不再持锁跨 `await`，同一个 cache 仍按顺序独占复用。活跃任务保持 3 秒检测，稳定空闲时按 3/6/12/30 秒退避，窗口恢复或用户交互会立即唤醒。
- 上游模型目录请求在请求级设置 12 秒总时限，并在读取 chunk 时强制执行 8 MiB 响应上限；解析结果最多接受 10000 个唯一模型，每个模型 ID 最多 512 个 UTF-8 字节。启动同步外层的 5 秒预算继续覆盖源配置解析和整个请求；配置页的交互同步不再使用短于双端点回退路径的前端伪超时，同一进程内由专用同步锁串行，避免超时后后台迟到写入与重试竞态。配置页目录合并使用线性 Set 去重，模型弹窗关闭时不构造内容，打开后支持搜索并按 200 项分批挂载，避免大目录一次创建全部 React 节点。
- 运行期 CDP bridge 将 websocket 读取、handler 执行和响应写回解耦：只读状态、模型目录、账号额度和插件列表最多并发执行 8 项，其他 API、懒加载以及会话导入导出仍进入单一串行通道；待处理队列上限为 256。协议代理入口只解析一次 Responses 请求 JSON，Chat SSE 转换器接管已拥有的请求对象；诊断日志通过 4096 项有界后台队列写入，按 64 条或 100 毫秒批量刷新，队列满时快速失败并在后续日志中记录丢弃数。rollout 头缓存的版本、provider 和条目未变化时不再仅因校验时间变化而重写文件。
- Codex Trace 写盘防护通过 SQLite `block_log_inserts` trigger 阻止 `logs_*.sqlite` 持续写入高频诊断日志；设置开关，已有日志和会话数据不会被删除。
- macOS Crashpad 磁盘保护与 Trace 共用诊断存储界面，但保持独立策略和开关。它只检查 `Application Support/Codex/Crashpad/pending` 与旧版 `Application Support/com.openai.codex/web/Crashpad/pending` 两个 allowlist 目录，不递归搜索其他产品数据；只把 UUID 命名的 `.dmp` 与 `_sidecar.json` 识别为同一报告组，跳过符号链接、未知文件、子目录及 Crashpad 的 `new`、`completed`、`attachments` 和设置文件。保护默认开启：启动时执行一次，此后每 5 分钟检查；总占用超过 512 MiB 时按最旧完整报告组回收到 384 MiB，至少保留最近 10 分钟写入。自动收敛不删除孤儿文件；手动清理可额外删除静默超过 24 小时的已识别孤儿。删除前后复核文件长度、修改时间及 Unix inode/device，消失或发生变化按并发竞争跳过。扫描、部分删除或后台任务失败只进入本地错误日志和诊断快照，不阻断 Codex 启动。
- Windows 默认开启新版卡顿补丁：Codey 在 Codex 主进程执行前通过仅绑定 `127.0.0.1` 的临时 Inspector，把会反复触发原生 DLL 加载失败的 `@worklouder/device-kit-oai` 替换为无设备桩，并断路每 30 秒启动一次的进程快照 Worker。已知 `child-process-snapshot-worker` 文件名或 `name: "child-process-snapshot"` Worker 语义名称会直接识别；文件改名、哈希化、改用 file/data URL 或 eval 且没有语义名称时，则读取有界 Worker 源码，并只在同时命中 PowerShell、`Get-CimInstance` / `Get-WmiObject`、`Win32_Process`、`Win32_PerfFormattedData_PerfProc_Process` / RawData 变体及 Worker 通信特征时断路；源码判定缓存采用最多 256 项的 LRU 淘汰，避免长期运行时随不同 Worker 路径持续增长。命中后直接返回合法空快照，不再启动 PowerShell；普通 Worker 和用户主动执行的 PowerShell 不受影响。替换 `worker_threads.Worker` 后还会同步 Node 的 ESM 内建导出，避免新版 Codex 通过 `import { Worker } from "node:worker_threads"` 绕过拦截。主进程保留 Worker 包装状态、ESM 同步状态、观察时长、源码检查与实际阻断计数，并通过现有 IPC 状态桥交给 Renderer 有界复核；界面只有在实际阻断过目标采样时才把该保护标记为已确认。观察窗口内没有匹配到目标 Worker 时仍保持待确认，并明确提示当前 WMI 来源可能尚未被识别。Inspector 随后立即关闭，不修改 Microsoft Store 安装目录。
- macOS / Windows 启动补丁会从 Codex app-server 的本次进程参数中移除 `--analytics-default-enabled`，追加进程级 `analytics.enabled=false` 覆盖，并在主 bundle 中显式关闭桌面主进程与 worker 的 CES 批量遥测，不改写用户配置。补丁同时移除 Codex 每 30 秒向当前 Renderer 拉取完整 app-state、仅写入调试日志与 Sentry breadcrumb 的诊断 heartbeat，并把每次 `browser-window-focus` 触发的外部插件状态检查合并为 30 秒 leading + trailing 节流，减少频繁切换窗口时对 Chrome profile、插件 marketplace 和本地清单的重复扫描；Renderer 就绪或显式触发的诊断快照仍保留，窗口内发生的插件变化仍会在尾部补做一次检查。每轮任务结束后的执行回收只处理本轮可安全重建的 `node_repl` helper；用户配置及插件提供的持久 MCP server 由 Codex app-server 管理，不再按 `kind = mcp` 或命令路径批量终止，避免下一轮重连时反复执行 `resources/list` 与 `resources/templates/list` 能力发现。
- Windows Git 请求保护会在 Codex 主进程启动前原位包装 Electron 的 `ipcMain.handle` / `handleOnce` 注册方法，并按消息内容识别 Git worker 请求和 Codey 状态探针，不再依赖 Codex 的具体 IPC channel 名；`electron` 与 `electron/main` 两种主进程入口都覆盖。这样 Codex 调整 channel 名或改用 ESM 导入时，后续注册的 handler 仍会被保护。同一包装层提供 Git 与 WMI 的只读状态握手；针对新版 preload 只等待 `ipcRenderer.invoke`、不再向页面返回结果的行为，主进程还会通过 Renderer 消息通道回传带请求 ID 的状态事件，页面只有收到匹配回执后才确认保护，不能把空返回值当作成功。旧客户端或主进程补丁降级时，Renderer 脚本仍尝试包装 `electronBridge.sendWorkerMessageFromView("git", ...)` 作为兼容回退；若 bridge 晚于注入出现，会使用有界退避重试。主进程与 Renderer 保护器只识别 `git-origins`、`status-summary`、`review-summary`、`branch-diff-stats` 以及包含这些只读查询的 `subscribe-live-query`；写操作、未知方法、其他 worker 和非 Windows 平台完全透传。首批请求使用容量为 3 的令牌桶通过，持续速率补充为每秒 1 个，同一仓库与查询键至少间隔 2 秒；等待队列总量封顶 48、单键封顶 6，最长等待 15 秒。尚未发送的请求收到原生 cancel 时会从队列移除。Renderer 回退还能对传输或可观察的 worker 响应失败执行最高 15 秒退避；两层都不伪造 Git 结果，也不缓存或合并不同 request ID，避免让 Codex worker 的 pending 请求失去对应响应。
- macOS / Windows 默认开启兼容型宠物精简：Codey 先把 Codex 自带的 `electron-avatar-overlay-open` 启动状态设为关闭，使宠物默认保持收起；Codex 设置页的 Pets 入口会在激活前按宠物专属语义 ID 屏蔽，设置 chunk 对 `codex-avatar` 的静态依赖替换成无资源桩，避免设置页预先载入宠物预览和内置精灵图，个人菜单和命令菜单中的宠物控件也继续屏蔽。主 bundle 中 Avatar Overlay manager 的启动预热会变成 no-op，普通启动不再提前创建长期隐藏的 `BrowserWindow`，因此该 renderer 也不会参与普通会话的 IPC owner 协调；manager、`initialRoute=/avatar-overlay`、专用 preload 与原生 `avatar-overlay.node` 仍保留，用户主动使用官方语音时可通过原生 presentation 路径按需创建。不得按窗口尺寸、`Pet Surface` 标题或 Avatar Overlay 通用 ID 全局拦截普通窗口。关闭开关后会在下一次由 Codey 启动 Codex 时恢复宠物、控件及原生预热，不改写 `app.asar`。
- 可选的 FastCtx 上下文优化默认关闭。没有现有 FastCtx 配置时，打开后会在下次启动 Codex 时把内嵌版本作为本地 STDIO MCP 临时注册，提供带分页和输出预算的 `inspect_local_file`、`grep`、`glob` 与 `replace` 工具，减少文件读取、搜索和机械替换产生的命令拼装与冗余上下文；无需另外安装 FastCtx、npm 包或 Node.js。检测到用户已经配置 FastCtx 时，设置页会禁用内置开关并通过悬浮提示说明原因，保存接口与启动配置层也会强制保持内置版本关闭，不复用用户 server、不注入 Codey FastCtx 指引。
- 可选的提示词优化默认关闭，独立于当前线路运行。用户可配置 OpenAI 兼容 API 地址、模型、凭据和自定义优化指令；配置热更新后 Codex composer 旁的按钮即时显示或隐藏。API Key 只保存在后端配置，渲染层只接收是否已配置的脱敏状态；优化日志不记录提示词正文或凭据。
- 可选的 Codey 子代理角色与调度增强默认关闭；它叠加在新版 Codex 默认启用的原生子代理能力之上，不再被描述为子代理总开关。打开后，Codey 通过公开 `[agents]` schema 写入 `enabled`、最大并发数、默认模型和默认推理档位，并暂时保留 `features.multi_agent_v2` 下的工具命名空间、等待参数、usage hint 与 Hook 兼容开关；用户已有的 `agents.interrupt_message` 原样保留，旧 `max_threads` 在新并发键缺失时迁移，`max_depth` 清理。随后注册 `codey_quick_scan`、`codey_deep_research`、`codey_visual_analysis`、`codey_worker`、`codey_visual_worker`、`default` 六个任务角色。`CodeyConfig.subagent_roles` 独立保存每个角色的模型与推理档位；旧配置缺少该字段时，把原有单一选择迁移到全部六类，部分角色缺失时从 `default` 补齐，未知角色丢弃。设置页只在开关开启时展示任务矩阵；每行复用当前线路模型目录并按模型元数据限制推理档位。普通模式继续使用 `config.toml` / `AGENTS.md` / `agents/default.toml` 租约，同时把六类角色的 Codey-owned 运行文件注册进临时配置；CC Switch Live 模式仅通过启动覆盖项注册相同文件，不写用户层 `config.toml` 或 `AGENTS.md`。生成 `model-catalogs/codey-official.json` 时保留本机官方缓存的 `multi_agent_version`：该字段只描述模型作为协调器的能力；合成第三方模型会移除模板继承的标记。Codex 0.147.0 起允许未标记为 V2 的 leaf model 作为子代理，因此角色候选继续包含当前线路全部可用模型，无需伪造协调器能力。线路切换或目录刷新时逐角色优先保留仍可用的选择，否则使用线路默认模型、Terra 或首个可用模型，并逐角色修正不支持的推理档位；目录暂时不可读或线路没有已选择模型时保留用户配置且不自动关闭增强。运行时始终注册稳定的六个角色文件路径；已启用状态下保存角色模型或档位时，在生命周期锁和运行代次复核内一次性重建六个文件，逐一验证 TOML，并在任一失败时恢复全部文件和租约。Codex 每次派生角色都会重新读取对应 `config_file`，所以下一次派生直接使用新配置，不重启 Renderer、app-server 或 Codex；首次启用或关闭、线路与 FastCtx 边界改变仍保留重启标记。正常退出或下次异常恢复时还原启动前内容，运行期间发生的独立用户修改会保守保留。
- 合成的第三方模型目录固定声明 `low`、`medium`、`high`、`xhigh` 四档推理强度，默认使用 `low`，不得继承本机官方模型缓存中的推理档位。Renderer 热刷新目录时也必须携带同一份第三方模型元数据，避免已打开页面继续沿用旧缓存中的单档能力。
- Windows 原生 EXE 启动会移除继承到子进程的陈旧 `WSL_DISTRO_NAME`，避免新版客户端无意同步探测 `wsl.exe`；用户在 Codex 中明确启用的 WSL 模式不受影响。
- 配置页提供“清理诊断存储”按钮：同一操作会在线清空 Trace 日志、截断 WAL 并压缩数据库，同时清理已稳定写入的 Crashpad 完整报告组；不会直接删除运行中仍被 Codex 持有的 SQLite 文件，也不触碰会话、账号、配置、插件或 Crashpad allowlist 之外的数据。Trace 与 Crashpad 分别返回清理结果，部分失败不会隐藏另一侧已经完成的回收。
- 诊断存储使用两个独立统计模块和一个组合刷新命令。Trace 快照展示日志条数、SQLite 实际占用和内容字节估算；Crashpad 快照展示目录、完整报告、文件、占用、时间范围和是否超过上限。两个 blocking 扫描并发执行并分别原子替换内存快照；配置页状态查询只序列化现有快照，不触发磁盘扫描。
- 侧边栏相对时间不再经 Codey bridge 查询 Codex SQLite。Renderer 复用已加载的官方 signal dispatcher，按 host 调用后台优先级的 `thread/list`，以官方 `recencyAt`、`updatedAt`、`createdAt` 顺序填充有界缓存；每轮处理最多 200 个当前可见任务，每个 host 最多读取 5 个 100 条分页，仍未命中的本地任务再按每批 32 个、4 并发调用 `thread/read(includeTurns=false)`，批次会持续排空而不是截断第 32 条后的任务。超过 200 条的待处理项由独立 pump 接续，单条精确读取失败只重试该项，分发器或列表整体失败则保留整批，二者均采用最多 5 次的有界指数退避。官方 signal wrapper 通过直接 app-server `sendRequest` 形状唯一确认；命名专用资产也只接受单一已知导出。发现失败时不猜测 `electronBridge` 的原生 IPC 协议，等待下一刷新周期重新发现。删除墓碑与无效官方时间会阻止旧缓存复活。
- 会话与插件修复在每次启动 Codex 前自动执行；普通模式的目标 provider 只读取得 Codex `config.toml` 当前活动值，根键缺失时按 Codex 规则使用内置 `openai`；CC Switch Live 模式只接受同一份已验证路由快照中的 provider。会话修复不会创建、重命名或切换 provider，也不会把悬空或高风险的保留 ID 写入历史。所有可解析 rollout JSONL 的 `session_meta.payload.model_provider` 与全部 Codex SQLite 中的 `threads.model_provider` 会永久对齐到该目标，并补齐 `has_user_event`；Provider 同步不得修改 `threads.cwd`、全局工作区根或按路径保存的偏好，避免 Windows 扩展路径、斜杠和盘符大小写变化导致历史被重新归入其他项目。没有可解析 `session_meta` 的残留或部分 rollout 同时被同步器与启动复核忽略，并按文件签名缓存，不会迫使每次启动重复全量同步。运行中切换 Live 线路会自动安全重启并重新对齐全部历史。Codey 不在退出时回滚这些会话改动，修复后直接启动原版 Codex 仍能看到历史会话。
- 启动官方 Codex 前会清理 `session_index.jsonl` 中既不存在于 rollout、也没有任何 SQLite 引用的精确格式幽灵任务。索引缺失或没有可清理条目时直接跳过，不再为此遍历全部 rollout 并对每个 Codex 数据库做全表扫描。首次解析会记录精确候选行身份，真正过滤时直接复用该计划，不再为同一 JSONL 做第二轮反序列化；重复 ID、未知结构、损坏行、CRLF 与无末尾换行保持原有语义。写入前保存原始索引并做快照一致性校验，备份位于 `~/.codex/backups_state/provider-sync`，保留最近 5 份 Codey 索引清理备份。
- 新版 Codex 的消息选择按 `data-turn-key` 选择整轮对话；Renderer 与后端会把 `history-content:turn:<turn_id>` 等 DOM 键归一成 rollout 使用的原始 `turn_id`，后端同时识别 `task_started` 与 `turn_context` 轮次边界并原地重写 rollout JSONL。当前最后一轮若仍使用 `history-content:tail:0:*` 页面临时键，后端只会在 rollout 的末轮边界与 `task_complete` / `turn_aborted` 终态一致时把它解析为稳定 `turn_id`，并在写墓碑前保存临时键到稳定 ID 的别名；同一次卸载后的二次清理和重复点击都会复用原 ID，不会把已经移动的“最后一轮”当成新目标。无法稳定解析时拒绝猜测，旧临时键也不会在后续启动时漂移到新的末轮。删除意图会先以不含正文的稳定轮次墓碑落盘，下一次启动在旧 Codex 已停止且新进程尚未恢复会话时重施，防止活跃内存延迟写回让已删上下文复活；未匹配到持久化轮次时页面不再先隐藏 DOM 制造删除成功的假象。旧版 SQLite 消息表继续兼容。
- 每条侧边栏会话提供数据导出按钮，生成带 `Codey会话-` 文件名前缀的可移植 `.codey-session.json`；导出时直接流式转义 JSONL 内容，不再为每行分配第二份转义字符串，并在序列化过程中强制执行 512 MB 传输上限，临时文件不会先膨胀到上限之外。会话列表标题栏兼容 Codex 的 `Tasks` 与 `Recents` 两代分区名称并提供全局导入入口，本地项目目录也提供导入按钮，可恢复完整 rollout 并将会话挂到目标项目。重复 ID 会自动导入为副本，不覆盖已有会话。
- 配置面板提供“恢复备份”，默认恢复最近一次会话数据库备份，也可通过 `restore_session_backup` 命令传入备份目录。
- 官方 curated 和本地工具插件市场通过 CodeyRuntime core 的兼容逻辑注册；`openai-curated-remote` 仅作为外部流程产生的可选本地缓存，缺失时不判为故障，存在时必须注册到其精确缓存路径。页面层合并可用的本地插件并清理隐藏/远程路径字段。
- 配置面板可保存用户脚本；脚本作为独立 CDP 文档脚本在内置修复脚本之后执行。

## Codey Workflow Engine 实施 RFC

> 状态：V1 opt-in preview 已落地，默认关闭；本节同时保留后续生产化目标，未标记为已实现的验收项不得视为已交付。
>
> 目标读者：Codey 维护者、测试人员和发布负责人。
>
> 产品优先级：准确性与端到端效率为一级目标，稳定性是不可牺牲的底线；Token 成本是重要约束，但不允许通过跳过必要验证来降低成本。

当前实现已经包含透明 App Server proxy、origin task 绑定、Direct/Guarded/Parallel/Expert DAG、SQLite Journal、幂等命令、租约与恢复、权限交集、最小上下文 worker、强制 FinalDelivery 写回、全局 composer 接管、可见原生回退和工作流控制台。首次启用需要重启以安装 proxy；全局普通文本接管可热切换。当前实现边界如下，所有缺口均失败关闭：

- Direct origin turn 的审批继续交给 Codex 原生界面；隔离 worker 使用 App Server `auto_review`。Codey 控制台尚未实现可持久停放并恢复的 server-initiated 审批交互闭环，因此 capability 仍明确报告不支持；auto-review 无法裁决时失败关闭。
- 自动 worktree 创建、合并与冲突处理尚未落地；脏工作区或高风险写入在接纳前拒绝工作流并允许可见原生回退，不执行自动 stash、reset、clean 或覆盖。
- Reviewer 使用显式 `PASS / CHANGES_REQUIRED / INCONCLUSIVE` 门禁；普通写入独立 Review，高风险 Expert 路径双 Reviewer，驳回后最多两轮 Builder 修复并使 Validator/Reviewer 证据整体失效重跑。修复耗尽、双 Reviewer 冲突或结论不明确时进入 NeedsAttention；尚未实现运行中的事后 Expert 升级，detached `review/start` 仍只是可选优化。
- outbox schema 与事务写入已存在，控制台当前通过 Journal 增量轮询读取；真实 Codex DOM/App Server E2E、跨平台故障矩阵、72 小时 soak 和 Token/准确率评测尚未完成。
- `reviewerCount` 与 `retentionDays` 已进入独立配置，但尚未接入 DAG 编译和自动保留期清理；普通/Expert 路径当前仍固定为一名/两名 Reviewer。V1 全局 writer 配额固定为 1，等待按仓库键隔离和 worktree 合并落地后才开放更高值。
- 当前接纳门禁验证 Journal、proxy 健康与 V1 所需协议形状；逐方法的运行时 capability 协商和跨 Codex 版本降级矩阵仍需在 canary 前补齐，能力不确定时必须关闭接管并显示原生回退。
- 已绑定 origin task 的 composer 预检通过 `thread/resume` 的 `excludeTurns` 只读取 cwd、权限、模型和线程状态，不得为了冻结权限快照装载完整历史；proxy 控制帧继续保留 1 MiB 防护。首次恢复若 legacy `sandbox` 投影暂时为空，会在短延迟后再做一次 metadata-only resume；仍为空时只接受 App Server 明确返回的 `:read-only`、`:workspace` 或 `:danger-full-access` 内置权限 profile，未知或自定义 profile 继续失败关闭。原生回退提示会展示后端原因，但在渲染前移除控制字符并限制长度，审计事件仍不保存请求正文或错误详情。

因此 V1 只能作为开发者 opt-in 预览，不得默认开启，也不得宣称已经达到本 RFC 后续列出的生产 SLO。

### 决策摘要

Codey 已参照 pi-shadow-mind、pi-dynamic-workflows 和 pi-maestro-flow 的架构思想实现原生、默认关闭的 Workflow Engine，但没有直接移植任一项目。Codex App Server 是首选执行适配层，现有单代理路径继续作为接纳前的兼容与回滚通道。

核心决策如下：

- 使用受版本控制的类型化 DAG 表达工作流，不执行任意 JavaScript 工作流代码。
- 使用 SQLite WAL 事件日志作为唯一权威状态；UI 状态、临时 marker、进程内存和 Hook 输出均不是事实来源。
- 采用至少一次事件投递、幂等键、事务 outbox、租约 epoch 和副作用栅栏，不宣称无法兑现的端到端 exactly-once。
- 原始用户请求不可变且优先级最高。Preflight 产物只能补充约束，不能覆盖、缩窄或改写用户目标。
- 角色、任务类型、模型路由和权限配置相互独立；权限必须由运行时强制执行，不能依赖角色名称或提示词。
- 接纳阶段把请求意图区分为 read-only、write 和 ambiguous。当前 Codex 权限快照始终是不可扩大的上限；只有明确咨询、审查或调研才收紧 write paths，无法确定的自然表达保留原上限并进入 Guarded，禁止因固定关键词漏判而永久降级为只读。
- Codex 页面中的工作流入口按当前 origin thread 精确查询：只有该任务存在至少一条关联运行时才挂载会话级按钮，切换任务时先隐藏旧入口再查询新任务。入口携带 thread ID 与 run ID 打开控制台，并把运行列表限制在该任务；终态运行仍可追溯，无关联任务不展示占位按钮。
- 所有非机械性写入默认经过独立 Reviewer；高风险写入使用双 Reviewer 或人工确认。
- 只读探索可以并行，仓库写入默认单写者。需要并行写入时必须使用隔离 worktree 和显式合并节点。
- Shadow reviewer 只作为可选的旁路建议器，不参与心跳、不成为正确性依赖，也不能阻止主流程恢复。
- 新引擎拥有独立角色配置并默认关闭；为避免二次调度，运行配置禁止与现有 subagent_optimization 同时启用，发布仍按观察、试用、灰度、默认候选四个阶段推进。
- 一旦工作流已接纳并可能产生副作用，不允许静默回退到旧路径；只能恢复、明确失败或进入 UnknownOutcome。

这不是对 Codex 核心的侵入式重写。Codey 负责计划编译、持久化、调度、权限、恢复和验收；Codex 仍负责模型推理、工具执行、审批交互和流式事件。

### 目标与非目标

必须达到的目标：

- 应用或后端重启后可以确定性恢复，不重复已经确认的写入。
- 网络超时、模型失败、工具挂起、审批中断、磁盘满、数据库异常、进程崩溃和取消竞态都有明确状态与处置。
- 对多文件检索、独立核验和测试任务自动并行，对写入冲突和上下文膨胀主动限流。
- 每个成功结论都有可追溯证据：输入、变更、测试、Reviewer 判定和最终交付彼此可关联。
- 任务使用最小必要上下文，不默认复制完整聊天历史或完整日志。
- 用户能够查看阶段、节点、阻塞原因、审批请求、重试、取消、恢复和最终证据。
- 未启用该功能时，当前 Codey/Codex 行为完全不变。

首版明确不做：

- 不建设跨机器的通用分布式任务平台。
- 不允许用户上传或执行任意工作流脚本。
- 不自动 stash、reset、clean、commit 或覆盖用户已有改动。
- 不把随机心跳、模型自评或提示词承诺当作安全边界。
- 不在一个版本内同时维护 App Server 与 SDK 两套完整执行栈。
- 不自动重放结果未知的写操作。
- 不保存模型隐藏推理；只保存结构化结论、必要证据和审计事件。

### 工作流模式与自动路由

工作流模式不是让用户每次手工选择的固定模板，而是由风险、并行收益和可验证性共同决定。

| 模式 | 适用条件 | 执行结构 | 质量要求 |
| --- | --- | --- | --- |
| Direct | 单一事实、明确位置、只读或完全可确定的机械操作 | 主代理内联 Preflight 后直接执行 | 必须有确定性检查；任何不确定性立即升级 |
| Guarded | 默认写入模式、单分支诊断、一般修复 | 独立 Preflight → Builder → 确定性验证 → 独立 Reviewer | 非机械写入必须 Reviewer 通过 |
| Parallel | 两个以上互不依赖的探索、核验或测试分支 | 并行只读 Scouts → 汇总 → 单 Builder → 验证 → Reviewer | 每个分支有独立证据和超时，写入仍串行 |
| Expert | 高风险、Reviewer 不一致、两轮修复后仍不确定 | Guarded/Parallel → Expert 针对单一争议给建议 → 人或主流程裁决 | Expert 不直接写入，不以建议替代测试 |

默认路由规则：

1. 任何非机械性仓库写入至少进入 Guarded。
2. 未知实现位置、跨目录检索或存在两个独立问题时，Preflight 使用 Parallel。
3. 涉及权限、安全、数据迁移、删除、发布、并发状态机或不可逆外部副作用时，进入高风险策略。
4. 高风险策略要求双 Reviewer；两者不一致时进入 NeedsAttention 或 Expert，不做多数投票式自动通过。
5. Direct 运行中一旦发现隐含依赖、工作区脏状态、测试失败或输出不可验证，重新编译为 Guarded，而不是继续冒险。
6. 机械格式化、生成文件刷新等操作只有在命令和预期输出完全确定时才可用确定性验证替代模型 Reviewer。

标准质量优先流程：

    OriginalRequest
      → Admission 与风险分类
      → Preflight / 并行 Scouts
      → WorkflowSpec 编译与策略校验
      → Builder
      → 确定性验证
      → 独立 Reviewer
      → PASS / CHANGES_REQUIRED / INCONCLUSIVE
      → 最多两轮修复，之后 Expert 或 NeedsAttention
      → FinalDelivery

### 总体架构

数据流：

    Codey React UI
      ↕ CDP bridge / Rust commands 与增量事件查询
    WorkflowService
      ├─ Admission / Planner / DAG Compiler
      ├─ Scheduler / Lease Manager / Policy Engine
      ├─ Artifact Store / Context Builder / Review Gates
      ├─ Recovery / Watchdog / Reconciler
      └─ Codex App Server Adapter
             ↕ 能力握手、任务、审批、取消、事件
          Codex runtime

    WorkflowService
      ↕ 单事务
    SQLite WAL Journal
      ├─ 状态快照
      ├─ 追加事件
      ├─ Transactional Outbox
      ├─ 幂等键与租约
      └─ Artifact manifest

组件职责：

- Admission：校验功能开关、输入、工作区、数据库、运行时能力和幂等键，失败时不创建半成品运行。
- Planner：把原始请求和只读 Preflight 编译为类型化 DAG；不直接执行副作用。
- Scheduler：只调度依赖满足的节点，控制读写并发、租约、公平性、重试和取消。
- Policy Engine：根据节点能力而非角色名称决定文件、命令、网络、审批和外部副作用权限。
- App Server Adapter：隔离协议差异，完成能力握手、事件标准化、去重、取消和恢复。
- Journal：所有已确认状态的唯一来源，支持确定性 replay 和版本迁移。
- Artifact Store：保存结构化产物及其 hash、敏感级别、来源和保留策略；大日志不进入调度上下文。
- Review Gates：把确定性测试证据和独立 Reviewer 判定组合成最终门禁。
- Reconciler：重启或连接中断后查询事实、回收租约，并把无法证明的副作用标为 UnknownOutcome。
- UI/API：只展示服务端权威状态，不自行推导成功。

### 类型化工作流 IR

WorkflowSpec 必须有版本号并在执行前冻结。首版至少包含：

| 字段 | 含义 |
| --- | --- |
| workflow_version | IR schema 版本；未知未来版本只能只读 |
| workflow_id / run_generation | 稳定运行标识和显式重跑代次 |
| original_request_ref | 指向不可变 OriginalRequest artifact |
| profile | direct、quality_first、high_risk 等策略配置 |
| workspace | 仓库、基线 revision、脏状态摘要和隔离策略 |
| nodes | 类型化节点集合 |
| edges | 显式依赖，不允许隐藏的字符串求值依赖 |
| acceptance | 最终测试、Reviewer 和人工门禁 |
| policy_snapshot | 接纳时冻结的模型、权限、重试和保留策略 |

NodeSpec 至少包含：

- node_id、task_type、depends_on 和 input_bindings。
- role、model_route、permission_profile、workspace_policy，四者分别配置。
- output_schema、artifact_outputs 和完成条件。
- timeout、retry_policy、criticality、cache_policy 和 side_effect_class。
- required_capabilities、approval_policy 和 cancellation_policy。
- 可选的 compensation 节点，但补偿不等于回滚成功。

输入绑定只允许引用已完成依赖的结构化字段，例如 node_name.output_field。禁止在模板中执行 JavaScript、Shell 或动态网络请求。DAG 编译阶段必须检查循环、缺失引用、权限矛盾、不可达节点和没有最终门禁的写入路径。

### Artifact 契约

所有跨节点信息使用 artifact，不默认传递完整会话历史。标准 artifact：

- OriginalRequest：原始用户请求、附件引用和接纳时上下文；不可变且优先级最高。
- PreflightBrief：范围、风险、未知项、建议验证和不应做的事情。
- ExecutionPlan：冻结后的 DAG 和策略快照。
- ChangeSet：修改文件、基线、diff hash、工具副作用和工作区状态。
- ValidationReport：命令、退出码、摘要、原始日志引用和可复现性。
- ReviewVerdict：PASS、CHANGES_REQUIRED 或 INCONCLUSIVE，附逐条验收映射。
- ExpertQuestion / ExpertAdvice：只包含一个明确争议和建议，不授予写权限。
- AdoptionDecision：主流程采用或拒绝建议的理由。
- FinalDelivery：面向用户的结果、证据、限制和剩余风险。

Artifact manifest 字段：

    artifact_id, kind, schema_version, content_hash, byte_size,
    producer_workflow_id, producer_node_id, producer_attempt_id,
    sensitivity, retention_class, created_at, storage_ref

敏感 artifact 默认不进入 Reviewer 或 Scout 上下文。任何摘要都必须保留来源引用；摘要不能覆盖原始 artifact，连续摘要最多三代。

### 状态机与事件模型

工作流状态：

    Created → Queued → Running → Succeeded
                              ↘ Failed
                              ↘ NeedsAttention
    Running → Pausing → Paused → Running
    Queued | Running | Paused → Canceling → Canceled

节点状态：

    Pending, Ready, Leased, Running, WaitingApproval,
    Pausing, Paused, Succeeded, Failed, Canceled, Skipped,
    UnknownOutcome, Compensating, Compensated

必须遵守的语义：

- CancelRequested 不等于 Canceled；只有执行侧确认停止且状态事务提交后才是 Canceled。
- Pausing 不等于 Paused；只有到达安全点并释放或冻结租约后才是 Paused。
- 传输错误不等于任务失败；执行结果未知时进入 UnknownOutcome。
- worker 消失不表示可以安全重试；必须先根据副作用栅栏和外部事实 reconcile。
- 终态吸收同一 generation 的迟到事件；用户显式重跑创建新的 run_generation。
- 每次重试创建新的 attempt_id，不覆盖旧 attempt。
- Reviewer 失败、超时或格式错误永远不能解释为 PASS。

每个持久事件至少包含：

    workflow_id, run_generation, node_id, attempt_id,
    event_id, workflow_seq, expected_version,
    causation_id, correlation_id, lease_epoch,
    payload_schema_version, actor, created_at, payload_hash

数据库唯一约束：

- workflow_id + workflow_seq 唯一，保证单运行内顺序。
- source + event_id 唯一，去除重复上游事件。
- workflow_id + node_id + attempt_id 唯一。
- lease 更新必须比较 lease_epoch，旧 worker 无法提交新结果。
- 状态转换、事件追加和 outbox 写入在同一事务中完成。

建议表：

- workflow_runs：运行快照、version、generation、策略快照和终态。
- workflow_nodes：节点定义、当前状态、依赖计数和最后 attempt。
- node_attempts：租约、心跳、执行句柄、结果、错误分类和副作用状态。
- workflow_events：不可变追加日志。
- workflow_outbox：待投递动作及其幂等键。
- artifacts：manifest 和存储引用。
- approvals：审批请求、响应、过期和关联 attempt。
- idempotency_keys：接纳、分发和外部副作用去重。

首版 SQLite 设置：

- WAL、foreign_keys=ON、synchronous=FULL、busy_timeout=5000ms。
- 每次启动先校验 schema、完整性和磁盘可写性；数据库损坏或磁盘满时停止接纳与写入，不自动创建一个空数据库冒充恢复成功。
- 未知未来 schema 进入只读安全模式，允许导出诊断，不允许变更状态。
- 活跃运行不自动清理；完成运行日志默认保留 30 天，去重与 tombstone 默认 90 天，详细工具日志默认 7 天，均可配置。

### 调度、租约和并发

质量优先默认值：

- 全局只读节点并发 4，同一模型提供方并发 2。
- 同一仓库写节点并发 1；并行写仅在独立 worktree 中开放。
- 子代理嵌套深度 1；首版不允许子代理继续派生子代理。
- 租约 30 秒，心跳 10 秒；45 秒无有效心跳后才允许带新 epoch 回收。
- 临时错误在 2 分钟窗口内最多重试 3 次，使用带抖动的指数退避。
- 逻辑失败和确定性测试失败不做基础设施式自动重试，而是进入修复或 Reviewer 流程。
- 写入、审批和最终 Review 默认禁止跨提供方自动 fallback，避免语义漂移；只读 Scout 可在策略明确时 fallback。
- 单一工作流最多两轮 Builder 修复和一个 Expert 节点，之后进入 NeedsAttention。

调度器使用 ready queue，只在全部依赖成功并通过输入 schema 校验后租赁节点。公平性至少按工作流轮转，避免一个大型 DAG 饿死短任务。高风险写节点在租赁前再次检查基线 revision、工作区锁和审批状态。

已有等价 artifact 且输入 hash、策略版本、模型路由和工具能力均一致时，可以复用只读节点结果。写节点不做结果缓存。预算耗尽只能暂停、降级非关键探索或请求用户选择，不能把未验证结果标为成功。

### 上下文与 Token 策略

省 Token 的主要手段是减少重复上下文，而不是减少必要角色：

- 每个节点只接收 OriginalRequest、直接依赖 artifact、当前策略和必要代码片段。
- Preflight、Reviewer 和 Expert 不接收 Builder 的完整聊天历史；Reviewer 接收原始请求、diff、验证证据和已知风险，以保持独立性。
- 搜索输出先结构化为 file:line、符号、结论和置信度；原始大输出存为 artifact，不重复注入。
- 相同静态前缀按 hash 复用；DAG 分支共享 artifact 引用，不复制正文。
- 上下文达到模型窗口约 70% 时生成一次带来源的压缩包，最迟 90% 前开启新 capsule；最多三代摘要。
- 每类节点设置输出上限和日志截断策略，但错误、权限请求、测试失败和 Reviewer 证据不得因截断消失。
- 并行只用于确有独立收益的分支；重复探索在证据已充分且结果等价时提前停止。
- Token 估算使用供应方 usage 或本地计量，只作为调度指标；不得用结果字符串长度冒充准确 Token。

模型不按角色固定写死。model_route 根据任务难度、风险、上下文长度、工具需求和当前可用能力解析为具体模型。质量优先配置下，Preflight、Builder 和 Reviewer 可以使用同等级高能力模型；低风险定位 Scout 才优先使用低延迟路由。

### 权限、审批和工作区隔离

默认权限矩阵：

| 角色 | 仓库读取 | 仓库写入 | 命令 | 网络/外部副作用 |
| --- | --- | --- | --- | --- |
| Coordinator | 元数据 | 否 | 否 | 仅调度协议 |
| Preflight / Scout | 是 | 否 | 只读白名单 | 默认否 |
| Builder | 是 | 按声明路径 | 按策略 | 逐能力审批 |
| Validator | 是 | 仅临时构建目录 | 测试白名单 | 默认否 |
| Reviewer | 是 | 否 | 只读或验证白名单 | 默认否 |
| Expert | 仅 artifact | 否 | 否 | 否 |

运行时必须依据 permission_profile 拒绝不允许的工具和路径。提示词中的“不要写文件”只是辅助，不是安全控制。Hook 只用于补充审计、脱敏和通知；由于并非所有执行路径都保证经过 Hook，不能把 Hook 当成完整权限边界。

工作区规则：

- 接纳时记录基线 revision、未跟踪文件摘要和已有修改 hash。
- 检测到脏工作区时不自动 stash、reset、clean 或 commit。
- 默认让用户已有修改留在原位，并用路径锁避免覆盖；高风险或并行写要求隔离 worktree。
- 合并前再次校验基线和目标文件 hash；冲突进入 NeedsAttention，不做模型猜测式覆盖。
- 外部副作用必须带幂等键；无法查询结果的非幂等副作用在断线后进入 UnknownOutcome。
- 破坏性动作、权限升级、发布和数据迁移必须走可恢复的显式审批。

### Codex 集成策略

MVP 首选 Codex App Server，因为它提供面向深度客户端集成的认证、历史、审批和流式事件接口。Codey 在启动工作流前执行能力握手，记录 App Server 版本和支持的方法；协议差异由 adapter 层处理，不能散落在调度器中。

能力握手至少确认：

- 创建或继续执行上下文。
- 发送任务和接收带稳定标识的事件。
- 工具审批与用户输入转发。
- 取消或中断执行。
- 查询运行状态，或在缺少查询能力时给出明确的恢复限制。
- usage、模型和上下文能力元数据。

如果必需能力缺失：

- 工作流接纳前可以明确回退到现有原生路径，并记录原因。
- 工作流接纳后不得静默换执行栈；进入 NeedsAttention 或兼容适配器的显式降级状态。
- 任何无法证明是否发生过写入的请求进入 UnknownOutcome。

Codex SDK 适合未来独立服务或自定义工具宿主，但 MVP 不并行建设第二套完整协议。Hook 用于观测和辅助策略，不能替代 App Server 事件协议和 Workflow Journal。

### 已落地的后端与前端模块

后端核心模块：

    backend/src/workflow/mod.rs
    backend/src/workflow/domain.rs
    backend/src/workflow/journal.rs
    backend/src/workflow/engine.rs
    backend/src/workflow/scheduler.rs
    backend/src/workflow/recovery.rs
    backend/src/workflow/policy.rs
    backend/src/workflow/artifacts.rs
    backend/src/workflow/app_server.rs
    backend/src/app_server_proxy.rs
    backend/src/commands/workflows.rs

需要集成的现有后端文件：

- backend/src/lib.rs 和 backend/src/main.rs：注册服务、生命周期和恢复入口。
- backend/src/commands.rs 与 backend/src/commands/runtime.rs：暴露命令并转发审批/用户输入。
- backend/src/launcher.rs：App Server 进程所有权与关闭语义。
- backend/src/config.rs：新增独立 workflow 配置及 revision/CAS 更新。
- backend/src/subagent_policy.rs：复用角色策略概念，但不把现有 subagent gate 当作权威状态。
- backend/src/codex_config.rs：只注入必要兼容配置，不把业务状态写入 Codex 配置。

前端与 composer 模块：

    src/workflows/types.ts
    src/workflows/api.ts
    src/workflows/snapshot.ts
    src/workflows/useWorkflowRuns.ts
    src/workflows/WorkflowConsole.tsx
    src/workflows/index.ts
    src/styles.workflows.css
    public/workflow-mode.js

需要集成的现有前端文件：

- src/api.ts：统一 API 封装。
- src/App.types.ts、src/App.tsx、src/main.tsx：页面入口和应用状态。
- src/overlay.tsx：只显示摘要，不复制完整控制台。

现有 marker 型 subagent gate 继续服务旧路径，但新引擎不得依赖它判断节点真相。迁移完成后再根据遥测决定是否删除旧 gate。

### Commands 与 UI 契约

当前 commands：

- workflow_capabilities：返回功能开关、协议能力、schema 版本和不可用原因。
- workflow_start：带 commandId 与 expectedRevision 接纳请求，返回 runId、engineEpoch、revision 和 durable ACK。
- workflow_steer：把当前任务的新消息转入活动 Builder；无法转发时写入带 commandId 的持久事件，并使受影响的 Builder 下游失效后重编译剩余 DAG。
- workflow_list：分页列出运行摘要。
- workflow_get：返回运行、节点、门禁和待处理交互。
- workflow_events：按 afterSequence 增量返回事件。
- workflow_artifact：按需读取脱敏并限长的 artifact。
- workflow_pause：持久化暂停意图。
- workflow_cancel：持久化取消意图，不虚报已取消。
- workflow_resume：只恢复 Paused 或可恢复的 NeedsAttention。
- workflow_retry_node：创建新 attempt；UnknownOutcome 默认不允许直接重试。
- workflow_reply_interaction：提交审批或用户输入，带 request version 防止重复响应。
- workflow_bypass_audit：只记录不含请求正文的原生绕过原因。

MVP 可用增量轮询：活跃运行约 1 秒、空闲 5 到 10 秒、页面隐藏时停止；后续若桥接层支持可靠推送，再替换为事件订阅。UI 必须显示：

- 当前模式、阶段、节点依赖和执行者。
- 运行中、等待审批、暂停中、取消中、UnknownOutcome 等真实状态。
- 最近事件、重试次数、租约/恢复提示和明确的阻塞原因。
- Token、耗时和并行度摘要，不用它们替代质量证据。
- 变更、验证和 Review artifact。
- 暂停、恢复、取消、重试和人工裁决按钮，并在危险操作前说明后果。

只有最终 acceptance gate 提交成功后才能显示 Succeeded。前端断线、轮询超时或 Reviewer 消失都不能显示成功。

### 初始配置

建议新增独立配置组，默认值如下：

| 配置 | 默认值 |
| --- | --- |
| workflow.enabled | false |
| workflow.globalMode | true |
| workflow.profile | qualityFirst |
| workflow.maxReadOnlyConcurrency | 4 |
| workflow.maxProviderConcurrency | 2 |
| workflow.maxRepoWriters | 1 |
| workflow.maxDelegationDepth | 1 |
| workflow.leaseSeconds | 60 |
| workflow.infrastructureRetryLimit | 3 |
| workflow.builderRepairLimit | 2（已接入自动修复循环，耗尽后进入 NeedsAttention） |
| workflow.reviewerCount | 1 |
| workflow.retentionDays | 30 |
| workflow.roles | 首次从现有角色配置复制，之后独立保存 |

配置更新沿用现有 revision/CAS 和原子写模式。运行接纳后冻结策略快照，配置热更新只影响新运行，避免一半节点使用新策略、一半节点使用旧策略。

workflow.enabled 与 subagent_optimization 的配置存储独立，但运行时互斥：

- 只启用 subagent_optimization：保持当前原生增强行为。
- 只启用 workflow.enabled：使用 Workflow Engine 自己的角色和调度策略。
- UI 开启任一能力时会关闭另一项；手工配置同时开启时，规范化阶段关闭 workflow.enabled，避免两个调度器递归派生。

### 异常分类与处置

| 异常 | 状态与动作 | 禁止行为 |
| --- | --- | --- |
| 接纳请求超时、无 ACK | 用 idempotency_key 查询 Journal；不存在才可重发 | 直接创建第二个运行 |
| 分发后连接中断 | 查询 App Server 和副作用栅栏；不能证明时 UnknownOutcome | 盲目重放写节点 |
| worker 崩溃或租约过期 | 新 epoch 回收；旧 epoch 结果只记审计不提交 | 接受迟到成功覆盖新 attempt |
| Codey/应用重启 | replay Journal、恢复 outbox、reconcile 执行句柄 | 依赖内存或 marker 猜状态 |
| 重复或乱序事件 | event_id 去重、workflow_seq 排序、version CAS | 重复推进状态机 |
| 取消与成功竞态 | 同一事务 CAS；已提交终态吸收迟到事件 | 把 CancelRequested 当 Canceled |
| 暂停遇到不可中断工具 | 等安全点；30 秒后 NeedsAttention 并说明仍可能运行 | 假装已经暂停 |
| Provider 限流/5xx | 退避重试；只读节点可按策略 fallback | 写节点跨模型静默重放 |
| 输出 schema 无效 | 一次结构修复；仍失败则节点 Failed/NeedsAttention | 把自由文本当结构化 PASS |
| Reviewer 超时或冲突 | INCONCLUSIVE；重试一次或进入 Expert/人工 | 默认通过 |
| 测试挂起 | 超时、中断、保存日志；副作用未知则 reconcile | 丢弃测试证据 |
| 脏工作区或基线漂移 | 停止写入、展示差异、隔离 worktree 或请求选择 | reset、stash、clean 用户文件 |
| 合并冲突 | NeedsAttention，保留双方 artifact | 自动覆盖冲突 |
| 数据库忙 | busy timeout 后退避；保持接纳幂等 | 绕过 Journal 执行 |
| 数据库损坏/磁盘满 | 停止新运行与状态写入，切只读诊断并备份 | 新建空库继续 |
| 未知 schema | 只读安全模式 | 降级写旧 schema |
| Artifact hash 不符 | 隔离 artifact，运行进入 NeedsAttention | 使用损坏内容 |
| 权限请求无人响应 | WaitingApproval；到期后 Paused/NeedsAttention | 自动同意 |
| 上下文接近上限 | 生成有来源 capsule 或拆分新节点 | 截掉错误与验收条件 |
| Secret canary 命中 | 立即停止外发、标记安全事件并清理派生上下文 | 继续 Reviewer/Scout 广播 |

取消要求：

- 取消意图持久化 p99 小于 500ms。
- 本地可控执行给予 10 秒协作退出，再给予 5 秒终止窗口。
- 终止后仍无法确认远程或外部副作用时进入 UnknownOutcome，不显示 Canceled。

### 验证、测试与评测

单元与属性测试：

- 状态 reducer 对所有合法和非法转换进行表驱动测试。
- 任意事件重复、乱序、进程中断后 replay 得到同一快照。
- 租约 epoch、CAS、终态吸收和 run_generation 的性质测试。
- DAG 循环、缺失依赖、schema、权限和最终门禁编译检查。
- Context capsule 的来源完整性、敏感级别和 Token 上限测试。

集成与故障注入：

- 在事务提交前后、outbox 投递前后、工具调用前后和结果 ACK 前后设置 kill point。
- 模拟数据库 busy、磁盘满、损坏、迁移中断和未来 schema。
- 用 mock App Server 覆盖超时、重复事件、迟到事件、审批、限流、断线、取消和结果未知矩阵。
- 用临时 Git fixture 覆盖脏工作区、未跟踪文件、基线漂移、worktree、冲突和部分写入。
- 覆盖 Reviewer 两轮震荡、双 Reviewer 冲突、Expert 不可用和人工接管。
- E2E 覆盖应用重启、活动运行恢复、暂停、取消、权限交互和 UI 断线重连。
- 发布候选至少执行 72 小时混沌与 soak 测试，并完成一次实际回滚演练。

质量评测集：

- 单一事实查找和简单只读诊断。
- 已知位置的小修改、跨文件重构和测试补全。
- 需求含糊、附件含伪指令、Preflight 与原请求冲突。
- 测试本身失败或 flaky、工具挂起、模型输出 schema 错误。
- 高风险权限、安全、迁移和不可逆外部副作用。
- 并行探索有收益与无收益的对照任务。
- 中途取消、应用重启、Provider 故障和脏工作区。

每个样本记录：任务成功、关键遗漏、错误成功、耗时、模型等待、工具时间、Token、重复上下文、重试、人工介入和恢复结果。必须与当前原生路径做盲评对照，不能只看模型自评。

### SLO 与发布门槛

稳定性门槛：

- 已 ACK 状态 RPO 为 0；replay 后状态确定一致。
- 不发生旧 lease 提交、重复破坏性副作用或用户工作区自动销毁。
- 协调层成功率至少 99.9%。
- durable ACK p99 小于 200ms，ready 到 leased p95 小于 1 秒，事件可见 p99 小于 2 秒。
- 重启恢复 p95 小于 10 秒、p99 小于 30 秒。
- UnknownOutcome 在无法确认后 60 秒内明确展示。

质量门槛：

- 关键评测集错误成功为 0。
- 成功的代码写入包含可追溯测试与 Review 证据比例至少 99.9%。
- Reviewer 不可用、测试缺失或 artifact 损坏时错误放行为 0。
- Secret canary 泄漏为 0。
- 相比当前原生路径，准确率不得下降；高风险任务必须有统计显著改善后才能进入默认候选。

效率与 Token 门槛：

- 调度层自身开销不含模型时间时，普通任务 p95 小于 2 秒。
- Parallel 模式在适合并行的评测集上显著降低端到端耗时；无并行收益时不得盲目扩散节点。
- Direct 任务总 Token 中位数不超过原生路径 1.1 倍。
- Guarded/Parallel 在同等或更高成功率下总 Token 中位数目标不超过原生路径 1.5 倍；若超过，必须能证明准确率收益并给出优化项。
- 重复上下文 Token 占比应持续下降，作为比“少派一个 Reviewer”更优先的成本优化指标。

### 分阶段实施

以下是原 RFC 估算，适用于从预览实现推进到生产验收的一名熟悉 Rust/CDP/React 的工程师；应在当前实现基线上重新校准。Journal、恢复语义和最终验收仍在关键路径上。

#### P0：协议与风险验证，3 到 5 天

- 固定三方项目参考 commit，完成许可证检查；默认只借鉴概念，不复制源码。
- 编写 ADR：App Server 优先、SQLite Journal、类型化 DAG、单写者和权限边界。
- 做 App Server capability probe，验证事件稳定标识、审批、取消、恢复和 usage。
- 对当前 subagent gate、配置 CAS、运行时进程和前端桥接建立基线测试。

完成标准：所有关键能力有实测证据；缺失能力有兼容设计；状态机、错误分类和 MVP 边界冻结。

#### P1：领域模型与持久化核心，7 到 10 天

- 实现 WorkflowSpec、NodeSpec、artifact 和事件 schema。
- 实现 SQLite migrations、WAL、append-only Journal、状态 reducer 和 transactional outbox。
- 实现幂等接纳、version CAS、lease epoch、generation 和 replay。
- 建立属性测试、迁移测试和 kill-point 基础设施。

完成标准：任意注入点崩溃后 replay 一致；重复/乱序事件不会重复推进；未来 schema 只读失败关闭。

#### P2：App Server Adapter 与恢复，7 到 10 天

- 实现进程监督、能力握手、协议兼容层、事件标准化和 dedupe。
- 实现审批、用户输入、取消、usage 和执行句柄持久化。
- 实现 Reconciler、租约回收、UnknownOutcome 和启动恢复。

完成标准：mock 故障矩阵全部通过；真实 App Server 上可完成只读节点、写节点、审批、取消和重启恢复。

#### P3：DAG、权限和质量优先流程，8 到 12 天

- 实现 Planner/Compiler、ready queue、读写并发和 worktree 策略。
- 实现 Preflight、Scout、Builder、Validator、Reviewer、Expert artifact 契约。
- 实现运行时 permission_profile、路径锁、副作用栅栏和最小上下文构建。
- 实现两轮修复、双 Reviewer、冲突和人工接管。

完成标准：Direct、Guarded、Parallel、Expert 四种模式均有 E2E；Reviewer 与 Builder 上下文独立；任何写入路径都不能绕过门禁。

#### P4：Commands、UI 与配置，5 到 8 天

- 实现 commands、after_seq 增量事件、审批和控制动作。
- 实现运行列表、DAG/阶段、证据、阻塞、恢复和 UnknownOutcome UI。
- 实现独立 workflow 配置、revision/CAS、默认关闭和运行策略快照。
- 补充诊断导出、只读安全模式和 kill switch。

完成标准：UI 与 Journal 状态一致；断线重连不丢事件；所有危险动作有明确语义和确认。

#### P5：评测、混沌和性能优化，7 到 10 天

- 建立原生路径对照评测集和自动报告。
- 完成数据库、进程、网络、工具、Git、审批和上下文故障注入。
- 优化 artifact 复用、上下文 capsule、批量事件提交和轮询。
- 运行 72 小时 soak、恢复演练和回滚演练。

完成标准：达到本 RFC 的稳定性、质量和 Token 门槛；没有未解释的错误成功。

#### P6：渐进发布，至少 1 到 2 周观察

1. Observe：只编译计划和记录路由决策，不执行工作流，不影响用户。
2. Opt-in：仅开发者和明确开启的用户；默认仍走原生路径。
3. Canary：按任务类型逐步扩大，先只读，再低风险写入，最后高风险门禁。
4. Default candidate：只有全部 SLO 连续满足且回滚演练通过后，才讨论默认开启。

每阶段都保留一个即时 kill switch。回滚只停止新接纳并让已接纳运行安全结束、暂停或进入 NeedsAttention；不得删除 Journal 或伪造终态。

### 交付清单

- ADR 与协议 capability matrix。
- Workflow IR、artifact、事件和权限 schema。
- Journal migrations、恢复 runbook 和诊断导出格式。
- App Server mock、故障注入库和对照评测集。
- 后端 Engine、adapter、commands 与前端运行控制界面。
- 配置迁移、kill switch、灰度指标和回滚手册。
- 面向维护者的故障处理文档。
- README 只以非技术语言描述已经可见的 opt-in preview，并明确失败关闭的用户边界；尚未交付的生产能力只记录在本文档。

### 首版待验证问题与默认取舍

| 问题 | 首版默认 | P0 验证点 |
| --- | --- | --- |
| Codey 页面关闭后谁拥有运行时 | MVP 要求 Codey runtime 活跃 | App Server 是否能被可靠重连和接管 |
| App Server 能否查询未知执行结果 | 不能确认即 UnknownOutcome | 执行句柄和状态查询能力 |
| 多窗口是否共享全局 writer lock | 共享同一 Journal 锁 | 进程间锁和崩溃释放 |
| 高风险写入是否自动创建 worktree | 默认提示并显式创建 | 用户体验、磁盘和 Git 兼容 |
| Artifact 是否需要静态加密 | 敏感内容最小化并按系统权限存储 | 威胁模型和密钥来源 |
| Provider fallback | 只读可选，写入默认关闭 | 语义一致性和 usage 统计 |
| Shadow reviewer | 默认关闭 | 是否带来独立质量收益而非噪声 |

这些问题不应通过提示词假设解决。P0 实测结果与本 RFC 不一致时，先更新 ADR 和验收标准，再开始 P1。

### 参考项目与采纳边界

- pi-dynamic-workflows 提供 phase、parallel、pipeline 和结构化终止输出的简洁思路；不采纳任意 JavaScript 执行、进程内状态、失败转 null 和字符串长度 Token 估算。[README](https://github.com/Michaelliv/pi-dynamic-workflows/blob/31b2aca0f1cb195aafbfc5e3ee2b8c83ad3f21a2/README.md) / [workflow.ts](https://github.com/Michaelliv/pi-dynamic-workflows/blob/31b2aca0f1cb195aafbfc5e3ee2b8c83ad3f21a2/src/workflow.ts)
- pi-maestro-flow 提供类型化 DAG、dependsOn、结构化输出、durable mailbox、retry classifier、circuit breaker、replay fence、context spill 和独立 verifier 的参考；不采纳全历史反复 fork、超时仅 detach 和强 Pi 耦合。[schemas.ts](https://github.com/catlog22/pi-maestro-flow/blob/30b9cccd780192e67fa44ddf348ace40a3ceefcc/packages/pi-maestro-teammate/src/extension/schemas.ts) / [retry.ts](https://github.com/catlog22/pi-maestro-flow/blob/30b9cccd780192e67fa44ddf348ace40a3ceefcc/packages/pi-maestro-teammate/src/runs/retry.ts) / [goal-verification.ts](https://github.com/catlog22/pi-maestro-flow/blob/30b9cccd780192e67fa44ddf348ace40a3ceefcc/packages/pi-maestro-flow/src/tools/goal-verification.ts)
- pi-shadow-mind 提供持久 Reviewer 角色、epoch 防迟到结果、净化 trajectory 和批量报告的参考；不采纳随机心跳、工具名 allowlist 安全边界和无工作区隔离的写入方式。[DESIGN.md](https://github.com/liuzhengdongfortest/pi-shadow-mind/blob/0fc4726aa9ca54fc7a25a3f4efa8114b4af29931/DESIGN.md) / [runtime.ts](https://github.com/liuzhengdongfortest/pi-shadow-mind/blob/0fc4726aa9ca54fc7a25a3f4efa8114b4af29931/src/runtime.ts) / [trajectory.ts](https://github.com/liuzhengdongfortest/pi-shadow-mind/blob/0fc4726aa9ca54fc7a25a3f4efa8114b4af29931/src/trajectory.ts)
- Codex App Server 是首选深度集成接口：[App Server](https://learn.chatgpt.com/docs/app-server)。
- Codex 子代理适合并行只读探索，但会增加 Token，写入并发需要谨慎：[Subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents)。
- Hook 适合做 guardrail 和观测，但不是完整边界：[Hooks](https://learn.chatgpt.com/docs/hooks)。
- Codex SDK 保留给未来独立宿主场景：[Codex SDK](https://learn.chatgpt.com/docs/codex-sdk)。

引用第三方项目时固定到已审阅 commit。没有完成许可证核验前只借鉴协议和架构思想，不复制源代码、提示词或文档。

## 运行时性能约束

- 后台会话扫描每轮仍枚举 `CODEX_HOME/sqlite` 以发现新增和删除，但会按数据库、WAL 元数据及 Unix 文件身份缓存 schema 探测结果；未变化候选不再重复打开 SQLite 查询 `sqlite_master`。已确认的会话库继续复用只读连接，近期会话查询使用连接级 prepared statement cache；数据库或 WAL 变化、同路径替换和 legacy `state_5.sqlite` 仍保持原有发现语义。
- CDP watchdog、重新注入和注入状态复核的周期错误日志通过 Tokio blocking pool 写入，避免文件锁、尾行修复和 flush 占用仅有的 async worker；启动、退出和恢复关键路径仍保留同步日志语义。
- 通知配置最多保存 32 个渠道，单个事件最多并发投递 4 个渠道；结果仍按渠道汇总，去重与不确定投递语义不变。
- 官方额度快照在后端成功缓存 30 秒；专用 mutex 合并同一时刻的 bridge 请求。失败后按 60、120、240、300 秒退避并封顶 300 秒，退避期间不重复读取 `auth.json` 或请求远端；成功后立即清除失败状态。

## 构建

需要 Rust 与 Node.js。首次构建前在本目录安装 `package.json` 中的前端依赖：

```bash
npm install
npm run check
cargo test --manifest-path Cargo.toml
npm run build
```

Windows 上执行 `npm run dev` 时，脚本只检查本次 Cargo profile 对应的本地 `codey.exe`。发现旧进程会先停止启动并要求从系统托盘或原终端正常退出，以便 Codey 清理 Codex 子进程和临时配置；只有确认进程卡死时才设置 `CODEY_DEV_FORCE_KILL=1` 重试。强制终止后会重新确认该进程已退出，确认失败时不会启动 Cargo。`npm run dev` 会先完整 `cargo build` 再 `cargo run`，确保 `codey-fastctx` sidecar 与主程序位于同一目录；直接手动 `cargo run` 前需要先 `cargo build`，否则本次启动会按未启用 FastCtx 继续并记录错误日志。

macOS 构建会同时生成无 Tauri 的 `target/release/bundle/macos/Codey.app`；直接打开该 App 即可启动 Codey。构建脚本会用最新 release 二进制重建并进行本地 ad-hoc 签名，避免继续运行旧包内的程序。

GitHub Actions 工作流 `.github/workflows/build-desktop.yml` 支持手动触发及推送 `v*` 标签触发。手动运行后可在 Actions 下载 macOS arm64/x64 未签名 ZIP 和 Windows x64 NSIS 安装程序；标签构建还会把这些文件附加到对应 GitHub Release。

### Cloudflare R2 更新分发

更新二进制可以发布到公开的 Cloudflare R2 bucket。标签发布时，工作流会先创建 GitHub Release，再将三个安装包上传至 `releases/<tag>/`，并分别写入版本化的 `releases/<tag>/latest.json` 和固定的 `latest.json`。清单包含版本、平台、包类型、下载链接、文件大小和 SHA-256；客户端默认使用项目公开的 R2 更新源，本地构建无需额外环境变量，发布构建仍可覆盖更新源。

先创建 R2 bucket，并为它绑定公开的 R2.dev 或自定义 HTTPS 域名；随后在 GitHub 源码仓库设置中配置：

- Actions variable `CLOUDFLARE_R2_BUCKET`：R2 bucket 名称。
- Actions variable `CLOUDFLARE_R2_PUBLIC_BASE_URL`：不带末尾 `/` 的公开 HTTPS 域名。构建时会写入 `${base}/latest.json` 作为更新地址。
- Actions secret `CLOUDFLARE_ACCOUNT_ID`：Cloudflare account ID。
- Actions secret `CLOUDFLARE_API_TOKEN`：仅授予目标 bucket `Workers R2 Storage: Edit` 权限的 API Token。

标签版本必须与 `package.json` 的 `version` 完全一致。本地发版脚本会同步 `package.json`、`Cargo.toml` 和 `Cargo.lock`，随后运行检查、提交、创建 tag 并推送到 GitHub：

```bash
pnpm run release -- 0.2.1
```

脚本默认要求工作区干净，避免把未确认改动一起发出去。需要把当前所有未提交改动放进这次发布提交时，显式使用：

```bash
pnpm run release -- 0.2.1 --include-existing-changes
```

可选参数：`--skip-checks` 跳过本地检查，`--no-push` 只创建本地提交和 tag，`--remote <name>` 指定推送远端。

未配置上述 variable 或 secret 时，现有 GitHub Release 发布不受影响，R2 同步会被跳过。默认构建使用项目公开的 R2 更新源；设置 `CODEY_UPDATE_BASE_URL` 可以在编译时覆盖该地址。配置页面不允许用户改写更新源。检查更新会经 HTTPS 拉取清单，校验版本、下载地址和 SHA-256 格式后显示是否有新版本；同一清单 URL 的检查结果缓存 30 秒，下载命令可复用 10 分钟内已验证的候选，网络或解析失败不写缓存，因而页面先检查再下载不会重复拉取清单。Codey 在恢复旧租约后、启动 Codex 前执行一次更新 preflight：检查超过 300 毫秒才显示无按钮的原生状态窗，10 秒硬超时、网络错误或清单错误均关闭提示并继续启动。Windows 状态窗运行在独立 Win32 消息线程；macOS 主线程运行 AppKit 事件循环，Tokio runtime 移到工作线程，状态窗使用不激活 Dock 图标的 `NSPanel`。发现当前平台可安装的新版本时使用原生自定义按钮询问；选择稍后会把本次结果保存在 `AppState`，renderer 从 `/backend/status` 恢复 Codey 图标红点，本次运行不再强弹，后续每 30 分钟只静默刷新红点。确认更新后复用同一次检查已验证的资产信息，显示下载校验状态，最长等待 300 秒；安装器成功拉起后直接退出 preflight，不进入 Codex 启动循环。下载、校验或安装器启动失败时提示错误并继续启动 Codex。当前 macOS 包仍是未签名包，Windows 包也尚未进行代码签名，因此不会静默下载或安装。

Codey 将运行时 core/data crate 固定在 `vendor/CodeyRuntime`，生命周期、会话扫描优化以及显式配置的独立协议代理句柄也已直接合并其中。主程序只复用该句柄和既有 Responses↔Chat 转换器，不接管 vendor 的整套启动器或全局设置。后端启动编排与 macOS/Windows/Unix 进程适配分层维护，运行时 TOML 三方恢复算法和私有原子文件 I/O 基元也已与 provider 应用/租约编排分离。本地与 CI 构建不需要额外的运行时源码目录或补丁。这些 crate 与后端同属根 Cargo workspace，`cargo test --workspace` 一条命令覆盖全部；统一依赖解析与特性合并消除了两个独立 workspace 的重复编译。PR 质量门在 Linux 上执行格式检查、完整测试及零警告 Clippy，Windows CI 补充该平台测试与 Clippy；桌面发布构建只保留 macOS 的 Rust 测试（macOS 无独立 CI 任务），Windows 的 Rust 检查由 CI 门保证，打包流程不再重复编译。

运行时只内置不含提示词的 Codex 模型兼容元数据，完整 system/developer prompt 不进入仓库资产或 CodeyRuntime 二进制。Codex 自定义模型目录的每个条目需要保留 `base_instructions`；本机官方 `models_cache.json` 可能直接提供该字段，也可能只在 `model_messages.instructions_template` 中提供等价模板。Codey 只从用户本机缓存派生运行目录，在本机写出前按默认 personality 解析模板并补齐旧版兼容字段，同时把生成文件权限收紧为仅当前用户可读写。缺少任一可用指令来源的本机缓存时不生成不完整目录，官方线路回退 Codex 内置目录，第三方线路仍可完成上游模型探测、手动模型选择保存与子代理能力校验；这是可恢复的内置目录回退，不记录为补丁失败。模型选择保存与线路模型同步必须只吞掉该明确的缓存兼容错误，目录读写或解析错误仍应返回给用户。这类本机派生内容不得写入日志、测试夹具、发布包或版本库。

## 配置与路径

- Codey 配置：由 `directories` 根据系统保存到 Codey 配置目录下的 `config.json`。
- 通知渠道在渠道弹窗确认后立即通过统一配置保存事务落盘并同步通知 watcher，不依赖控制台顶部的二次保存；安装更新前还会提交当前未保存设置，避免更新重启丢失仅存在于渲染器内的草稿。
- cc-switch 配置：数据库用于判断 CC Switch 路由接管状态、匹配直连线路的协议提示，以及在有效路由接管下为模型目录请求只读解析当前源 API；不会替代 Codex 配置成为活动线路来源，也不会持久化源凭据。`CC_SWITCH_DB_PATH` 指定的数据库文件优先级最高；否则读取 cc-switch Tauri Store 中的 `app_config_dir_override` 并跟随其自定义数据目录，未配置覆盖时使用 `~/.cc-switch/cc-switch.db`。Windows 还与 cc-switch 一致：仅在默认数据库不存在时兼容旧版 `HOME/.cc-switch/cc-switch.db`。
- Codex 配置：使用 Codex 默认 `CODEX_HOME`（通常是 `~/.codex`）。
- Trace 写盘防护由 `disableTraceLogWrites` 控制，默认开启；macOS / Windows 使用相同启动时机更新 Codex 根目录及旧版 `sqlite/` 目录中现有的 `logs_*.sqlite`，不会创建、清空或压缩日志库。macOS Crashpad 容量保护由独立的 `protectCrashpadPending` 控制，默认开启且保存后热切换；Windows 保留兼容配置字段但不扫描 Crashpad 目录。
- Windows 卡顿补丁不设开关：Codey 在运行时识别 Windows，并在每次启动 Codex 时自动隔离 Micro 设备模块和周期性 WMI 进程采样。启动成功只表示主进程补丁已安装；WMI 保护通过独立运行时证据区分等待首次采样、已实际阻断和观察窗口内未匹配到可识别目标，只有实际阻断才确认生效，不能再用安装结果直接宣称 WMI 已修复。启动 Codey 时若目标 Codex 主进程已在运行，会先终止该安装目录下的 Codex 进程树，确认退出后再拉起新主进程，确保补丁能在主进程执行前安装；清理失败会中止启动。macOS 不执行 Windows 专属分支。
- 宠物精简：`slimCodexPet` 默认为 `true`，在下次通过 Codey 启动 Codex 时生效。启用后默认收起宠物、隐藏宠物专属入口、精简设置页预览资源，并跳过 Avatar Overlay 的启动预热；共用 manager 和语音能力仍保留，只有主动使用语音时才按需创建 Overlay。关闭后下次启动会恢复完整宠物功能和原生预热。
- 浮动额度：`showAccountUsageInHeader` 默认为 `true`，保存后立即生效且不要求重启。只有活动线路被识别为官方账号登录时才请求并展示，切到第三方线路后保留开关值但停止请求和显示；用户手动关闭后的持久化值不会被默认值覆盖。
- Codex 慢启动保护：`fastCodexStartup` 默认为 `true`。Codey 会在 Electron 主进程仍处于启动暂停阶段时，为登录后的 Statsig bootstrap 设置 1.5 秒上限，并保留 renderer 保护作为兼容兜底；正常响应保持原流程，慢请求或失败请求会让 Codex 使用自身错误降级路径继续挂载主界面。原始初始化仍可在后续刷新中恢复；关闭后下次启动完全使用 Codex 原生等待策略。
- FastCtx 上下文工具：`fastContextTools` 默认为 `false`。设置页与运行时使用同一套独立 token 规则检查 `mcp_servers`，普通 table、子项 inline table 和根 inline table 都会检查；只要非 Codey-owned server 的 ID、`command` 或 `args` 命中 `fastctx`（大小写不敏感），就返回 `fastContextToolsStatus.userConfigured = true` 和对应 `serverId`。读取、UTF-8 或 TOML 解析失败时改为返回 `detectionFailed = true`，仅把 FastCtx 开关锁定为关闭并显示悬浮原因，不阻断其他设置的加载和保存。通用保存接口会再次检测并强制把 `fast_context_tools` 归一化为 `false`，启动配置层也保留同一防御；用户 server 完整保留，Codey 不注册内置 server、不注入 FastCtx 指引，也不把外部 namespace 写入子代理配置。带 `--codey-fastctx-mcp` 标记的 Codey-owned server 无论使用普通或 inline TOML 都能在关闭时删除；重新启用时会规范为普通 table，同时保留已有的非托管字段和环境变量。
- 未检测到外部 FastCtx 时，Codey 才在本次运行的临时 `config.toml` 中注册随 Codey 分发的 `codey-fastctx` sidecar 作为独立的本地 STDIO MCP。FastCtx 及其 o200k 分词器数据只编入该 sidecar；sidecar 保留 `--codey-fastctx-mcp` 作为 Codey 自有注册标记，并把上游 `runtime-bootstrap` / `runtime-host` 子进程交给 CLI 分发。Codey 设置 8500 token 的 FastCtx 预算，并在用户没有配置 Codex 工具输出上限时设置 10000 token；内置 namespace 会从 `features.code_mode.direct_only_tool_namespaces` 临时移除，使工具既能直接调用，也能出现在 code-mode `tools` 对象中，普通 table、根 inline table 和子项 inline table 都执行同一处理。FastCtx 0.2.5 的读取函数为 `mcp__codey_fastctx__inspect_local_file`，文件路径传绝对路径；FastCtx 只发布四个精确命名的直接工具。根代理和默认子代理的共享指引按任务明确分流：普通本地文件读取、文本搜索、文件发现与机械替换优先 FastCtx，并覆盖通用 `rg`/shell-first 规则；CodeGraph 只用于符号、调用者/被调用者与调用链等语义代码理解；构建、测试、Git、包管理以及 FastCtx 确实不可用或失败时才使用终端。工具尚未暴露时应先通过 `tool_search` 加载其直接工具；若 code mode 没有暴露 `tool_search`，则从 `ALL_TOOLS` 定位精确函数后经 `tools` 调用，不能直接回退终端，也不能为本地工作区虚构替代服务器。为避免负向枚举反而诱发无效调用，当前模型指引不再逐个点名宿主提供的通用 Resources helpers，旧版指引仍作为一次性迁移模板保留。开启内置 FastCtx 时还会注册一个 `PreToolUse` 路由 Hook，matcher 精确覆盖 canonical `Bash` 与三个通用 Resources helper：对于终端命令，仅当可保守确认由纯读取、搜索或文件发现片段组成时拒绝，并返回对应 FastCtx 函数；重定向、写入型 `find`/`sed`、未知片段、构建、测试、Git 和包管理均放行。对于 Resources 调用，Hook 会拒绝把 Codey FastCtx server/namespace 当作资源服务器、缺少真实 server/URI、无 URI scheme、Unix/UNC/Windows 绝对路径等无效读取，并拒绝显式空 server 的定向发现；无 server 的全局发现、其他服务器的定向发现以及带合法 URI scheme 的远程资源读取继续放行。FastCtx 确实不可见或失败后，可把 `# codey-fastctx-fallback` 作为命令第一行显式回退；该注释同时兼容 POSIX shell、PowerShell 与 WSL。
- 历史 FastCtx 提示词采用一次性持久迁移：创建运行时租约和原始快照前，Codey 会从磁盘基线中的根 `developer_instructions`、`features.multi_agent_v2.subagent_developer_instructions` 和 `agents/default.toml` 删除所有已知旧版 Codey 固定提示词，其中包括 0.2.4 及更早模板和曾随 0.2.5 写入但缺少 Resources 边界说明的模板；动态外部 namespace 的旧 Codey 模板也会迁移，普通与 inline 子代理配置使用同一清理规则。写入前重新核对原始字节，避免覆盖同时发生的用户修改。内置 FastCtx 启用时只在本次运行配置中追加当前 0.2.5 指引，子代理默认配置同步使用同一内置 namespace；退出恢复的是已经完成迁移的干净基线，因此任何已知旧提示词都不会复活，当前提示词也不会在 MCP 不可用时永久残留。运行时临时配置真正落盘前还会复核 `config.toml`、`AGENTS.md` 和 `agents/default.toml` 均未发生并发变化；部分写入失败时统一使用已保存租约做三方恢复，而不是无条件覆盖回启动前字节。关闭或被外部 FastCtx 阻断时仍会幂等清理当前运行配置中的 Codey-owned server、完整提示词块和可共同确认归属的 `mcp__codey_fastctx` namespace；用户其他提示词、server、无法证明归属的 namespace 和输出上限保持不变。
- 提示词优化：`promptOptimization.enabled` 默认为 `false`，打开后即时生效且不要求重启。配置使用脱敏保存的 `baseUrl`、`apiKey`、`protocol`、`model` 和可编辑 `instruction`；界面直接展示内置默认指令，空持久化值仍由后端回落到同一默认值。第三方线路可调用独立命令一次性读取活动 profile、Codex 本地配置与 CC Switch 当前源，复制真实 URL、Key、协议和默认模型并立即持久化；官方登录线路不展示同步按钮，后端也会拒绝同步。同步返回 renderer 前仍清空 Key，只保留 `apiKeyConfigured`；依赖额外请求头但没有标准 API Key 的线路要求手动配置独立接口。测试和模型列表命令可使用未保存草稿并回填被脱敏的 Key，请求按保存的 Chat Completions 或 Responses 协议发送，Responses 结果兼容 `output_text` 与标准 `output[].content[].text`。`optimize_prompt` 只从 renderer 接收待优化文本；输入最多 32K 字符，优化结果最多 8192 字符。响应与错误预览使用流式有界读取，非成功 HTTP 状态按失败返回，404 自动补 `/v1` 重试后以重试响应作为最终诊断。前端同步、测试和模型列表操作互斥，前端兜底超时长于后端请求时限；composer 观察器在输入控件尚未找到、已经断开、导航事件或与 composer 相关的 DOM 变化时重新扫描，无关页面 mutation 不触发全局查询与布局检查。
- Codey 子代理角色与调度增强：`subagentOptimization` 默认为 `false`。关闭时设置页只显示开关与说明；开启后显示“快速定位、深度检索、视觉分析、代码实施、视觉实施、通用兜底”六类固定任务，每类都有用途提示、独立模型和推理档位。候选由受支持的 `officialModels` 与全部 `thirdPartyModels` 组成；新版 Codex 将其中不具备协调器标记的模型作为 leaf model 使用，因此不再设置静态 V2 白名单。线路切换、启动和手动刷新模型目录时逐角色校验，已保存模型仍可用时保留，否则依次尝试线路默认模型、Terra 和首个可用模型，推理档位仅在目标模型不支持时回退。旧版单一 `subagentModel` / `subagentReasoningEffort` 配置首次加载时会无损扩展到六类，`default` 角色继续同步这两个兼容字段。已启用运行时保存角色配置或模型选择导致角色回退时，会原子刷新整组六个运行文件并更新 applied snapshot；成功后清除这部分 `restartRequired`，失败则保留旧运行文件并返回热更新错误。首次启用、关闭或其他运行边界变化仍要求重启。角色 TOML 中的 `sandbox_mode` 只定义默认权限；Codex 会重新应用父任务当前的实时 sandbox / approval 覆盖，因此界面和文档不得把角色名描述为独立安全边界。
- 子代理约束文件与路由：可编辑源文件为 `codex-constraints/subagent.toml` 和 `codex-constraints/agents/<role>.toml`，Codey 只对内容仍精确等于历史内置模板的旧根文件执行一次性迁移；用户修改过的指令不会再靠全文检索替换。根约束会以 AGENTS.md 的名义显式请求主动委派，并把未知位置的跨文件检索、两个以上独立分支、大量外围材料和边界清晰的独立实现定义为必须派发条件，避免 Codex 的保守协作默认策略把“用户未点名”当成不派发理由；已知小文件、即将修改的确切代码、奠基性文档和单一事实仍由主代理直接处理。每次启动会把源文件、当前 FastCtx 指引和设置页选择的模型/档位合成为 Codey-owned `codex-constraints/runtime/` 副本，用户只编辑源文件，不直接编辑运行时副本；角色设置热更新复用同一合成和校验流程，并保持注册路径不变。普通模式在临时 `config.toml` 中注册六个 `[agents.<role>]`；CC Switch Live 隔离模式不写用户 `config.toml` 或 `AGENTS.md`，而是通过进程级 `-c agents.<role>.config_file=...` 和 description 覆盖项引用这些副本，并在目录可用时用绝对路径覆盖 `model_catalog_json`。该目录保留官方协调器标记并让第三方合成模型保持未标记 leaf 状态，所以切换线路不会覆盖 Codey 约束、污染 CC Switch 配置或夸大模型能力。根代理路由提示按任务选择 `agent_type`，模型与推理档位由对应角色 TOML 固定。
- 子代理等待门禁与 Hook：Codey 显式开启 `features.multi_agent_v2.wait_agent_enabled`，并把等待上限设为 120 秒。根代理先完成同一批独立任务的派发，再调用 `agents.wait_agent`；mailbox 的 `MESSAGE` 或其他局部更新可用于 `agents.send_message`、`agents.followup_task`、`agents.interrupt_agent`、`agents.list_agents` 等必要协调，随后继续等待。在每个已派生代理产生 `FINAL_ANSWER` 或 `task_complete` 前，仍禁止非协作本地工具和根任务结束。Codey 注册 `PreToolUse`、`PostToolUse`、`SubagentStart`、`SubagentStop`、`Stop` 与 `SessionEnd` 六类同步 command hook；普通模式把 Hook 定义及精确信任哈希放入临时租约，CC Switch Live 模式则把 Hook 合并到独立 `~/.codex/hooks.json`，仅把稳定路径计算的信任哈希作为进程覆盖项。已有用户 Hook 保持原顺序与内容，Codey 只增删带自身命令标记的 group。运行状态按 `session_id` 和 `agent_id` 隔离；子代理不能继续派生，根代理有活动子代理时只能调用 `agents.*` 协作工具，`PostToolUse` 与 `Stop` 会在尚未汇合时维持门禁。根代理的 `wait_agent` 被用户输入或手动停止中断时，Hook 会清理该 `session_id` 的全部活动标记，让恢复后的任务脱离已经无法汇合的旧子代理；随后迟到的 `SubagentStop` 按幂等事件处理。状态目录随这项语义调整升级为 v2，升级前遗留且无法验证的 v1 活动标记不会继续阻塞会话。Hook 状态读取失败对根代理 fail-closed。旧 `[agents]` 迁移把 `max_threads` 转成 `max_concurrent_threads_per_session`、移除 `max_depth` 并保留 `interrupt_message`；`features.multi_agent_v2.expose_spawn_agent_model_overrides` 保持关闭，防止派生调用绕过逐角色模型配置。
- Codex App 路径：留空时使用 CodeyRuntime 的平台发现逻辑。Windows 自动发现失败或已保存路径失效时，会在启动阶段打开原生目录选择器并持久化规范化后的应用目录，因此自定义盘符不依赖尚未启动的 Codex 页面；配置页只展示当前解析结果，不提供无法在首次启动失败时触达的恢复弹窗。目录解析兼容安装根目录下的 `app`、`bin`、`current` 与 `versions/current` 布局。
- CDP 默认端口：`9229`，如 Windows 端口被占用会按 core 的逻辑选择可用回环端口。

- FastCtx 路由 Hook 会对每个命中的 `PreToolUse` 独立执行；拒绝原因只保留目标函数与显式回退标记，完整的工具发现、code mode 和 Windows 路径规则由运行时 FastCtx 指引统一提供，避免连续读取时在 Codex 钩子面板重复刷出整段说明。

### 通知渠道扩展

通知实现按“公共调度 + 渠道适配”拆分。后端 `backend/src/notifications/` 中的配置、事件、格式化和调度器不依赖具体渠道；每个发送渠道放在 `channels/` 的独立文件中，实现 `NotificationChannelAdapter`，并在 `channels/mod.rs` 注册。新增渠道时需要同时补齐渠道枚举与配置字段、请求构造、明确的成功响应校验、传输与响应错误脱敏及对应单元测试；HTTP 成功但响应损坏或缺少渠道成功字段仍按发送失败处理。

前端 `src/notifications/` 以 `channelRegistry.tsx` 为唯一渠道注册入口，每个渠道使用独立编辑器组件；注册项负责显示信息、默认配置和完整性判断，公共列表只负责展示、编辑和删除，启用状态与测试发送都在渠道编辑弹窗内配置。新增和编辑必须先完成渠道配置，并经不落盘的 `test_notification_channel` 测试成功后才能保存；每次修改草稿都会要求重新测试。外部配置结构继续使用 `webhook.channels`，既有 `test_webhook` 仍保留以兼容已有渲染层调用和持久化数据。涉及凭据的渠道必须保持普通配置返回渲染层前脱敏、留空保存时回填旧值、显式清除时不回填；仅在用户主动打开某一渠道编辑弹窗时，可经 `reveal_notification_channel` 按需返回该渠道凭据，弹窗关闭后立即清空本地草稿。

## 启动与恢复

运行时配置的应用、失败回滚与退出恢复由 Codey 配置目录中的跨进程文件锁串行化；锁覆盖租约快照、最终字节复核和原子替换，避免两个 Codey 进程在“检查后写入”窗口互相覆盖。外部编辑器不会遵守该锁，因此恢复逻辑仍必须按 original/applied/current 三方内容只撤销 Codey-owned 字段，不能把这把锁描述成文件系统 CAS。CC Switch Live 目前仍需把门禁 Hook 临时合并到稳定的 `hooks.json`，因为 Codex `app-server` 不接受 profile 选择且 session flag Hook 不能自行取得信任；Electron 启动补丁只给 Codey 管理的 app-server 注入 `CODEY_SUBAGENT_GATE_ACTIVE=1`，门禁 helper 在其他 Codex 会话中固定返回空结果，不执行等待或派生限制。Windows 进入 WSL 启动链时，命令级覆盖中的盘符路径会在 shell 注入前转换为 `/mnt/<drive>/...`，原生 Windows 启动参数保持不变。

设置保存接口按 JSON 请求中字段是否真实出现来合并子代理配置：缺少或传入空的 `subagentRoles` 时保留已有逐角色选择；旧版 `subagentModel` / `subagentReasoningEffort` 只更新 `default` 兼容角色；非空的部分角色 map 只覆盖请求中给出的角色。完整新客户端仍可一次更新全部六类。`default` 探索角色与三个探索/分析角色一样显式使用 `sandbox_mode = "read-only"`，只有两个实施角色使用 `workspace-write`。

打开 Codey 后不会创建常驻原生配置窗口；仅当 Windows 无法解析 Codex 应用路径时，启动阶段会显示一次系统目录选择器。Codey 会先恢复上次租约并同步当前线路；CC Switch Live 模式随后建立并校验不可混配的路由快照，普通模式则只读取得 Codex 当前活动 provider。只有目标 provider 验证完成后才永久同步 rollout 与 SQLite、清理幽灵任务索引，接着备份并临时应用运行时配置、修复插件市场、启动 Codex，最后通过 CDP 注入轻量控制脚本；会话修复本身不会改写活动 provider。Windows 和 macOS 启动时会按目标主可执行文件判断 Codex 是否正在运行；命中后先终止同一安装目录下的主进程、Helper、app-server 及后代进程树，确认退出后再由 Codey 拉起，清理失败则中止启动。首次 Codex 启动失败时，Codey 会调用与正常退出相同的运行时停止和配置恢复逻辑，失败后等待 100 毫秒重试一次；Windows 随后通过阻塞任务显示原生错误对话框，用户关闭对话框后当前 Codey 进程返回错误并退出，不进入常驻关闭等待。首次点击 Codex header 中的 “Codey” 按钮时才会加载紧凑 React 浮层，配置操作通过本次 CDP bridge 发送给 Rust 进程。遮罩空白处、右上角关闭按钮和 `Esc` 都能关闭浮层。关闭这次由 Codey 拉起的 Codex 后，Codey 会先标记退出、取消并等待尚未执行完的延迟重启任务，再停止路由 watcher，终止该实例拥有的 Codex 主进程、Helper、app-server 及后代进程树，恢复临时配置并自行退出；收到系统退出信号和安装更新时也执行同一套清理。退出阶段不再按 Codey 可执行文件路径扫描或终止其他实例，进程所有权以当前 runtime 保存的 PID、进程组和启动身份约束。会话 JSONL、数据库与索引清理结果不回滚。若 CDP 注入失败，Codey 会停止本次启动、显示原始错误并退出，不会另起本地 Web 服务。

Codey 不改写 `auth.json`，因此 Codex 的账号栏仍会显示原来的官方登录账号；这只代表客户端登录会话，不代表第三方 provider 仍走官方接口。读取 Codex 活动线路时，provider 范围内的 `experimental_bearer_token` 优先于 `auth.json` 中的 API Key。非路由模式运行期间当前 provider ID 保持不变，第三方 API 地址、协议和 bearer token 会在 Codex 启动窗口内写入该 provider 的临时配置；对于由 cc-switch 协议提示识别的 Chat Completions 线路，首屏就绪后磁盘 provider 表恢复为启动前真实地址，运行中的 Codex 与本地协议代理继续使用已加载快照。内置 `openai` 官方线路继续使用 Codex 自身的 provider 定义，不写入无效的保留 ID 覆盖。第三方线路若错误使用 Codex 保留 provider ID 会在启动前被拒绝并提示改用非保留自定义 ID；路由模式则完整保留 CC Switch Live provider 表与接管 token。

如果 Codey 异常退出，下次启动前会检查 `codex-lease.json`；所有新格式租约都对启动前原始内容、Codey 已应用内容和当前内容做三方合并，只撤销 Codey-owned 字段。非路由模式下用户在运行期间手动改写的 provider ID、API 地址或同表扩展会原样保留，FastCtx、模型目录与推理档位等临时 overlay 仍会清理；路由模式同样保留 CC Switch 最新 Live 内容，避免切换 provider 后因保护性早退而遗留 overlay。当前线路切换统一由 watcher 触发受控重启，不再生成 `route-snapshots`；恢复代码仍识别旧租约中的 rebased snapshot 路径，确保升级前异常退出的运行实例可以收尾。缺少已应用快照的旧租约只回滚旧版本明确拥有的 provider、模型目录、推理档位与 FastCtx 字段，并保留插件、市场、用户新增键及同表中的并发扩展，不再整文件覆盖或删除当前配置。路由 watcher 每秒先比较文件元数据，只在变更、待确认切换或每 30 秒兜底校验时读取并解析完整配置。Codey 自身的所有 `config.json` 读改写事务共用一把异步写锁；整份设置保存还携带持久化的 `settingsRevision`，旧页面或并发请求提交的过期快照会被拒绝。配置保存前发生的 Trace 防护和模型目录写入都保留可回滚快照，持久化失败时恢复外部状态，避免磁盘、内存与运行时配置分裂。启动备份目录采用保留策略：应用运行时配置前清理 `codex-backups` 下最旧的启动备份，保留最近 5 份及当前租约引用的目录。

## 已知限制

- 目标是 Codex Electron 桌面客户端，不覆盖 CLI。
- 子代理等待门禁建立在 Codex 的本地 command Hook 路径上，可覆盖 shell、`apply_patch`、MCP 和大多数本地 function tools；Codex 托管的 WebSearch 不经过 `PreToolUse` / `PostToolUse`，个别专用工具路径也可能选择退出默认 Hook 路径，因此该门禁是编码流程的确定性本地保护，不是覆盖所有托管能力的安全边界。并发提交到同一批次、且早于 `SubagentStart` 活动标记建立的工具调用仍由根代理 usage hint 约束。
- Windows 新版卡顿补丁针对 Codex Micro / Work Louder 设备集成导致的原生模块异常，以及当前客户端的周期性 WMI 遥测采样；Windows 上会自动启用，不会连接 Codex Micro 硬件，命中已知文件名、Worker 语义名称或完整源码特征的遥测 Worker 时也不会启动对应 PowerShell。插件 app-server 在清理旧进程时可能执行的一次性 WMI 查询仍保留，避免产生孤儿进程；它不是 30 秒反复调用的来源。主进程安装 Worker 包装器并同步 ESM 内建导出后会执行一次同步自检：使用私有 Symbol 标记的合成构造参数走同一包装器，并确认返回安全空采样 Worker；该自检不会创建原生线程、子进程、定时器或 PowerShell，也不计入真实阻止次数。自检通过即可确认保护有效，真实目标尚未触发时单独显示等待状态；后续命中仍展示实际阻止次数和识别来源。自检失败则明确报告失败，旧主进程没有自检字段时仍保留 45 秒观察窗兼容诊断。状态快照只暴露最近 Worker 的 basename、清洗后的线程名称和源码信号名称，不暴露完整路径或数据值。配置面板仅在旧版兼容待确认状态下做最长 60 秒有界复核，不常驻轮询。Git 请求保护优先在 Codex 主进程的 Git worker IPC handler 上限流，并通过只读 IPC 握手向 Renderer 生效探针报告状态；旧客户端仍保留 Renderer bridge 兼容回退。主进程保护能覆盖所有进入该 Git worker handler 的目标请求，但无法拦截 Git worker 或原生 app-server 已经接受订阅后在内部自行触发的刷新，因此它是降低请求风暴速率的前置保护，不是 Windows 内核资源异常的完整修复。配置面板只在 Git 状态仍为“已执行但未验证”时做最长 30 秒的有界复核，不常驻轮询。兼容型宠物精简与 FastCtx 上下文工具保留用户开关。
- 当前 Codex 优先按 `threads.rollout_path` 定位 JSONL，并按 `task_started.turn_id` 删除整轮记录；旧版 `messages`、`thread_items`、`items` SQLite schema 作为兼容路径。
- 内嵌 FastCtx 当前只发布文件读取、搜索、发现与批量替换工具，不发布 MCP Resources 接口及其可选 Bash/后台任务组。Codex 只要初始化了任意 MCP server 就会注册通用 Resources handlers，当前配置 schema 不能按名称隐藏这几个内建工具；Codey 因此通过让内置 FastCtx 同时进入 direct 与 code-mode 工具表，避免 code mode 在看不到正确函数时退回通用 Resources 路径。Codey 注入到根代理和默认子代理的规则只正向说明应调用的 FastCtx 函数，并在直接工具尚未可见时要求先走 `tool_search`；执行前 Hook 负责拦截 FastCtx 资源误路由及占位 URI，避免模型指引反复点名无关工具。URI 形态的本地引用会先规范化为普通绝对路径，再直接交给 FastCtx `inspect_local_file` 工具。PDF 引擎未编入 Codey，PDF 应继续使用 Codex 自带的 PDF 能力。
- 第三方线路可以提供 Codex 原生支持的 Responses API，也可以提供 OpenAI 兼容的 Chat Completions API；后者由 Codey 在运行期间通过临时回环代理完成 Responses↔Chat Completions 转换。原生 Anthropic、Gemini 等其他协议不在适配范围内。
- 页面注入使用稳定的 `data-*`/`electronBridge.sendMessageFromView` 探测，Codex bundle 大幅改版时可能需要更新选择器适配层。
- 消息通知按渠道列表保存，支持同时配置多个飞书 Webhook 与 Telegram Bot；旧版单飞书配置在读取时自动迁移。飞书接受官方或企业内网主机名的 HTTPS 机器人地址，仍要求 443 端口、标准 `/open-apis/bot/v2/hook/...` 路径且禁止 URL 用户信息、查询参数和片段；通知专用 HTTP 客户端不跟随重定向。`session.completed` 由真实 Codex turn 的完成状态触发，不再把单次模型 HTTP 响应误判为任务结束；失败、等待介入与手动测试仍保留。自动通知会并发投递到所有已启用且配置完整的渠道，并汇总失败；只有连接拒绝或渠道明确返回失败等确定结果才会自动重试，HTTP 超时、响应读取中断及其他没有明确失败响应的传输错误一律视为远端可能已经接收，停止重试并保留本次去重记录。等待介入通知采用写前持久化去重：先原子记录预留再请求渠道，确定失败时回滚；因为飞书与 Telegram Webhook 都没有可依赖的幂等键，进程在预留后、确认响应前崩溃时会保守地抑制重发，边界为 at-most-once。waiting 去重台账按插入序持久化并封顶 2048 条，超出时淘汰最旧键；台账写盘在阻塞线程执行且不占用状态锁。完成/失败通知使用当前进程内的有界去重历史，不承诺跨进程 exactly-once。飞书不保存或发送签名密钥；飞书 Webhook 地址与 Telegram Bot Token 默认不会返回渲染层，并通过配置状态保留已有凭据。用户主动打开单一渠道编辑弹窗时，后端才会临时回显该渠道凭据，弹窗关闭即清空本地草稿。所有通知消息都不包含 prompt、正文、内部会话 ID、线路 ID 或 API Key。
- 首版明文 API Key、飞书 Webhook 地址与 Telegram Bot Token 仅依赖配置文件权限保护，后续可把 `ConfigStore` 的 secret 存取替换为 macOS Keychain/Windows Credential Manager。

FastCtx sidecar 外层监督器只缓存并重放无副作用的 MCP 初始化握手。控制中心 transport 中断时，所有在途请求都会收到明确的未重放错误，worker 随后重建并继续同一客户端连接；`replace` 等可能写入的调用绝不自动重放。worker 的 transport 退出使用专用状态码，并在错误日志中标记为可恢复。

FastCtx 集成基于 [yc-duan/fastctx](https://github.com/yc-duan/fastctx) `0.2.5` 的固定提交 `774704a`（Apache-2.0）。
