# Codey 内部开发文档

本文档面向 Codey 的开发和维护，保留实现细节、构建发布流程、配置路径、启动恢复机制和已知限制。面向使用者的功能介绍只维护在 `README.md`；不要把协议、端口、路径、构建命令、数据库结构、补丁策略或其他内部技术细节迁回公开 README。

Codey 是一个无界面的 Rust 桌面辅助进程，通过 CDP 连接官方 Codex Electron 客户端，并把 React 配置控制台直接注入 Codex 页面内的隔离浮层。官方线路与原生 Responses 第三方线路仍由 Codex 直接连接；OpenAI 兼容的 Chat Completions 第三方线路会在 Codex 运行期间启用仅绑定回环地址、使用随机端口的临时协议代理。第三方运行时 provider 配置和代理生命周期均由 Codey 管理，退出后停止代理并原子恢复启动前的配置。

## 当前能力

- 启动器的 `CodeyRuntime::start()` 只负责编排七个有序阶段：诊断存储保护、线路快照解析、启动前存储维护、运行时 Provider 配置、补丁与路由监听、进程启动及首屏注入、运行期 watcher 安装；阶段顺序、错误记录、失败恢复和 receiver 返回语义保持不变。macOS / Windows 的 Electron 启动补丁源码独立维护在 `backend/src/codex_startup_patch.js`，Rust 通过 `include_str!` 编译进二进制，前端检查会先执行 Node 语法校验。共享 bridge 统一提供 Statsig 客户端发现、React 内部键枚举以及可配置祖先深度的 fiber 图检索；模型白名单、宠物与语音盾牌不再各自实现 React host 扫描。模型配置 hook 在源头用 `useCallback` 发布业务回调，根 `App` 直接把这些回调传入 memo 子组件，不再为同一组回调逐个建立 ref、layout effect 和外层 callback。
- 打开 Codey 时自动启动 Codex，并通过 CDP 注入 Codey 设置按钮、Fast 模式展示修复、插件市场修复和消息选择工具；设置按钮在 Codex 客户端内部打开 Shadow DOM 隔离的 Semi Modal 配置浮层，不跳转外部浏览器。
- 配置页运行状态卡通过 `runtime_status` 展示 Codey 版本、Codex App 路径、Codex App 版本和维护状态；`codexAppVersion` 优先读取当前受控 runtime 的应用目录，其次读取用户保存的应用路径，不在普通状态轮询里做全系统发现。
- Windows 原生 EXE 使用 GUI 子系统，运行期间不会创建命令行窗口。首次启动 Codex 失败时，Codey 会恢复临时配置、显示系统错误对话框并退出；清理失败时，对话框和诊断日志会同时保留启动错误与清理错误。
- 线路同步始终直接读取 Codex `config.toml` 中的活动 provider，并从 provider token 或 `auth.json` API Key 取得凭据，不再根据 CC Switch 数据库选择地址、凭据或活动线路。唯一例外是已同时确认管理态与 Live 标记的 CC Switch 路由：模型列表请求会只读解析数据库中的当前源 API 地址和凭据，构造不持久化的临时请求配置，线路运行配置仍以 Codex 为准。该路径兼容用户手工维护以及 CC Switch 已写入 Codex 的第三方地址、环境变量和请求扩展字段；provider 范围内的 `experimental_bearer_token` 优先于 `auth.json` 中同时保留的 ChatGPT OAuth，不得把明确的第三方地址误判为官方线路。CC Switch 路由关闭时，其部分 OpenAI Chat 线路为了满足 Codex 配置约束仍会写成 `wire_api = "responses"`；Codey 只在 Codex 当前直连 `base_url` 与 CC Switch 数据库当前线路的配置地址或 `provider_endpoints` 精确匹配时，读取不含凭据的 `meta.apiFormat` 作为协议提示。`openai_chat` 启用 Codey 协议代理，`openai_responses` 保持原生直连；数据库缺失、旧 schema、解析失败、手工地址不匹配或活动地址为回环代理时均忽略该提示。若活动 provider 以精确的 `name = "OpenAI"` 声明支持 Codex 远程压缩，Codey 会把该能力随线路配置持久化并在临时 provider 中保留。线路变化需要重启由 Codey 启动的 Codex 后生效。
- 官方线路沿用 ChatGPT 登录；原生 Responses 第三方线路继续把 API 地址和临时 bearer token 直接交给 Codex。OpenAI 兼容的 Chat Completions 线路启动时会生成一份不依赖全局状态的显式配置快照，把 Codex 的 Responses 请求经临时回环代理转换为 Chat Completions 请求，并把普通响应、SSE 文本流、工具调用、用量和错误转换回 Codex 所需格式。代理使用系统分配的临时端口并跟随受控 Codex 进程启停；本地 listener 最多同时保留 64 个连接，请求头与请求体分别限制为 64 KiB 和 32 MiB，请求读取使用 15 秒 idle timeout 与 45 秒总 deadline，长时 SSE 响应本身不施加总时限。上游请求复用同一个带连接池的 HTTP client，并单独施加 5 秒连接超时和普通/流式响应头超时；启动或运行时配置失败会立即回收。原生 Anthropic、Gemini 等非 OpenAI 兼容协议仍不在该适配范围内。
- 官方账号线路默认开启浮动额度展示。额度组件以固定定位浮窗挂在 Codex 右下角，默认保留 24px 边距，套餐、周期额度、余额和本地刷新时间纵向展示；拖拽结束后把浮窗 `left/top` 保存在 Codex renderer 的 localStorage 中，并在窗口尺寸变化时约束回可视范围。轻量 renderer 每 60 秒通过 Codey bridge 请求一次额度快照；Rust 后端只在当前 provider 判定为官方且 `showAccountUsageInHeader` 已开启时读取 `auth.json` 的 ChatGPT access token 和 account ID，请求 ChatGPT backend 的 `/wham/usage`，并兼容 `/api/codex/usage` 旧路径。渲染层只接收已归一化的周期、使用比例、重置时间、方案和余额，不接收 OAuth 凭据；第三方线路、关闭开关或请求失败时会自动隐藏组件或保留上一次成功结果并标记为过期。
- CC Switch 路由接管通过数据库 `proxy_config.enabled`、Codex 的 `proxy_live_backup` 及旧版 `proxy_takeover_codex` 设置识别管理态，并用活动 provider 的 `PROXY_MANAGED` 标记或 `cc-switch-official` 回环地址验证 Live 接管态。启动时从同一次 `config.toml` / `auth.json` 读取中建立 Live 路由快照，活动 provider 必须存在对应表并带有效 HTTP(S) 地址；第三方线路不得使用 Codex 保留 provider ID。管理态存在但 Live 标记缺失、provider 悬空或地址无效时，Codey 在会话同步和代理启动前停止，提示用户关闭后重新开启路由。有效接管下沿用快照中的 provider、地址、凭据和协议，跳过 Codey 模型目录刷新以及 `model_provider`、`model_providers`、`model`、`model_catalog_json` 写入，只叠加推理档位、FastCtx、子代理等 Codey 自有运行时字段；活动地址为 CC Switch 回环代理时不会应用数据库中的 Chat 格式提示或启动第二个 Codey 协议代理。路由关闭且直连线路匹配 `openai_chat` 提示时，Codey 才把临时 provider 的 `base_url` 与 `wire_api` 覆盖为自己的 Responses 协议代理，同时保留真实上游地址和凭据用于 Chat Completions 转换。
- 配置页以官方账号可见的 7 个模型为固定左列；每次拉起第三方线路前会在 5 秒上限内请求 `/v1/models` 或 `/models`，非路由模式使用 Codex 当前 provider，CC Switch 有效接管时则绕过回环代理、只读解析其当前源 API 地址和真实凭据。源地址必须是非回环 HTTP(S) 地址，`PROXY_MANAGED` 只能作为接管标记，绝不作为 bearer token 发往源服务；解析失败按普通模型同步失败处理。同步成功后仅向 Codex 展示上游支持的模型，无需再手动同步并重启。请求失败、超时或返回空列表时优先沿用该线路上次保存的模型支持配置，首次使用且尚无保存配置时才回退到固定 7 模型并继续启动。配置页手动同步失败后仍会打开模型弹框，明确提示线路可能不支持模型目录接口；弹框始终列出 7 个官方模型供用户勾选，并允许输入其他模型 ID。其他模型输入会在前后端同时拒绝官方清单中的模型，保存时把官方勾选与其他模型共同写为该线路已确认的支持范围。“测试对话”会对当前第三方线路和默认模型发起一次真实的最小请求，独立验证认证、端点、模型和所选 wire API，不在后台自动消耗额度。模型支持范围、上游目录或默认模型保存后，后端会通过当前 renderer 的 CDP 连接把新目录直接传给模型白名单 `setCatalog()`，避免保存请求内部再次调用 bridge 形成重入等待；renderer 同时改写 Statsig 模型配置、触发 `values_updated`、刷新 React Query 的 `models/list` 活跃缓存，并在 app-server 返回旧目录时于消息捕获阶段替换模型描述。模型白名单还会在 `thread/start`、`thread/resume`、`turn/start` 以及宿主的直接和包装 IPC 请求发出前，把缺失或已经不属于当前目录的模型替换为当前默认模型，避免线路切换后继续发送旧 GPT 模型。后端除校验 `snapshot()` 的模型顺序与默认模型外，还要求命中 Statsig 订阅和当前模型查询缓存才把本次保存报告为立即生效；运行时模型基线仅在这些校验成功后更新，因此模型变更可单独清除重启标记，热刷新失败时则保留重启要求。
- 启动前备份 Codex `config.toml`，退出时按 lease marker 原子恢复，`auth.json` 和官方登录状态保持不变。租约同时记录本次 Chat Completions 协议代理地址。应用临时配置时以启动路由快照做 CAS 校验，并在真正拉起 Codex 前再次核对 `config.toml` 与 `auth.json`，防止启动准备期间发生切线。CC Switch 路由模式从租约应用完成后每秒检查一次 Live 配置与认证；外部内容稳定两个检查周期后不再把新 provider 指回旧协议代理，而是触发受控 Codex 重启。停止流程先结束 watcher 和旧 Codex，再关闭旧协议代理并按三方合并恢复最新 CC Switch 基线；新启动重新读取完整路由快照、同步会话并按需建立新协议代理，因此 provider、端点、token 和 wire API 不会跨快照混用。
- 启动器对 `sessions` 与 `archived_sessions` 的 rollout 采用逐行流式检查；只有确实需要改写 provider 的文件才会载入全文，避免长会话历史在启动时形成多份大字符串并把内存峰值长期留在分配器中。
- 启动器只读取 rollout 的首个 `session_meta` 头并流式遍历目录，不再为校验构建全量路径列表；头部校验按目录分片到最多 4 条线程并发执行，任一目录发现 provider 不匹配即整体提前结束。Trace 防护、Crashpad 容量收敛、插件维护和宠物状态会在依赖关系允许时并行执行；诊断存储统计只在用户请求时执行。Provider 迁移、陈旧锁恢复、应用目录解析、模型目录读写、所有 Codey 配置落盘、运行时 TOML 应用以及启动前、失败回滚和停止阶段的配置恢复都通过 blocking worker 执行，避免计划重启、失败清理或保存设置时阻塞仍存活的 async bridge；周期 watcher 写错误日志时使用可等待的 blocking 包装，退出与启动关键路径仍保留同步写入以保证落盘语义。恢复任务仍按原顺序等待完成，不会与进程回收或协议代理关闭并发。启动流程把初始 Renderer 注入、失败清理、watchdog 创建及跨平台进程停止收敛为独立 helper，保持原有失败恢复和 watcher 关闭顺序。配置写锁继续覆盖 CAS 校验、外部副作用、持久化和内存发布，以维持 revision 及磁盘/内存一致性。应用目录解析完成后，启动器先停止并等待旧 Codex 进程退出，再执行 rollout/provider 同步与会话索引清理，避免永久维护和仍在写入的 Codex 竞争；模型目录准备随后在 blocking worker 中执行。官方模型目录在同一次启动内按文件大小和修改时间复用解析结果，不再为 `refresh_for_provider` 和 `selection_state` 各解析一遍。
- Codey 的受控基础脚本会预构建为单个 CDP 文档注入包并在健康恢复时复用，默认注入从 16 次脚本往返降为 2 次；共享 bridge 必须先于慢启动保护执行，并统一提供 Statsig 客户端发现、React fiber 遍历和带优先级的单点 fetch 拦截注册。fast、voice 与插件市场脚本不再层叠覆盖 `window.fetch`，重复注入只替换同名拦截器，最后一个拦截器撤销后恢复原生 fetch。约 689 KB 的 React 设置浮层、按需组件样式与主题变量只在首次点击 Codey 按钮时注入，用户脚本仍保持独立且最后执行。`public/` 注入脚本在 `vite:build` 阶段压缩到 `dist-overlay/inject/` 后才嵌入二进制；布尔占位符通过数组取值阻断 esbuild 的解析期常量折叠，构建脚本仍逐文件校验占位符幸存并在异常时回退源码，测试同时锁定占位符和压缩收益。浮层 CSS 会剔除所有逗号选择器都带 `-rtl` 类的独立规则，与 `body`/`:host` 共享选择器列表的主题变量块保持原样；本地 Badge 不再静态引入 Semi Tag 及其 Avatar 样式，Card、Modal 的传递依赖在没有产物级视觉白名单前不做选择器盲删。额度组件在数值未变化时跳过 DOM 重建；CDP 注入重试采用约 30 秒总预算内的指数退避，为新版 Windows Codex 较慢的 Renderer 资产准备保留实际注入时间；每 60 秒的额度刷新会记住上次成功的接口端点，失败时仍回退完整列表。
- 配置页的通知、确认框和 Codex 路径弹窗各自使用独立的外部 store；只有对应的 memo host 通过 `useSyncExternalStore` 订阅，提示文本、确认内容和路径输入变化不会重新执行根 `App`。根组件只保留跨面板的配置、运行状态、busy、portal 与诊断快照状态。运行状态响应在写入前按值复用未变化的维护、注入与诊断子快照，完全相同的轮询结果不会提交 React 更新；更新卡、功能策略和运行面板只接收各自需要的稳定切片，诊断或重启轮询不会再穿透这些 memo 边界。
- `codey-errors.log` 继续只记录失败，但所有新增启动诊断统一携带 `stage`、`durationMs`、`attempts`、`timeoutMs` 和 `recoverable` 字段；旧版主进程补丁 helper 记录仍可兼容读取。协议代理的上游请求、响应、失败与故障转移日志会记录线路、端点、模型、wire API、HTTP 状态、尝试次数、超时和是否继续故障转移；传输失败只记录白名单化的 `errorClass`，不持久化上游自由文本错误、bearer token、API Key、请求正文或用户提示词。手动“测试对话”失败同样只记录线路、模型、wire API、状态和响应长度。CDP 注入使用约 30 秒硬 deadline，失败时写入真实重试次数和耗时，不再使用固定估算。
- Renderer 启动时只保留设置按钮与轻量侧边栏探测；导入、导出、删除、相对时间和消息选择等会话工具要等用户首次悬停、点击或键盘聚焦侧边栏后才加载，加载完成后会撤掉启动探测观察器。增量观察器按新增控件最近的会话行、项目行、侧边栏分区或消息轮次修复，刷新前再次合并祖先/后代根节点，且仅在顶栏确实变化时重找设置按钮；节流在持续变更下最多推迟 250 毫秒，避免流式输出把刷新无限期饿死。命中消息轮次的根节点只跑消息选择安装器，不再对整轮子树重复执行侧边栏安装器；会话 ID 探测只在用户真正硬删过消息后才进行，消息选择按钮按行缓存而非每次全子树查找。相对时间只遍历已登记的会话行并跳过无变化的 DOM 写入，窗口回前台触发的强制刷新按 10 秒去抖。观察器不监听流式正文的 `characterData` 或无业务消费者的 `style` 变更；`class` 仍用于识别原生会话运行态与 spinner，不得移除。插件 bridge 使用有界指数退避等待宿主接口，也不会再序列化无关 IPC 的完整参数，并在解析请求体前先做子串预筛，避免为无关请求整体 `JSON.parse`。
- 宠物与语音屏蔽脚本不会跨扫描缓存 React fiber 判定：React 可能复用 host element 并独立替换 props/fiber，voice 还必须识别被复用后已不再属于语音功能的控件；性能由 bridge 的单个 document-root `MutationObserver`、合并后的 `attributeFilter`、有界根队列和帧调度控制。pet、voice 与完全访问权限提示共用该观察器，最后一个订阅撤销时才断开；pet/voice 还复用 bridge 提供的控件描述归一化、控件子树查询、事件拦截与 teardown 骨架，但各自保留独立队列和生命周期。renderer 启动观察器会在会话工具接管后断开，正式会话工具观察器仍按生命周期接棒，不并入盾牌分发器。完全访问权限提示只扫描新插入的子树并改用 `textContent`，不再每次触发整页按钮遍历和布局刷新。模型白名单的交互重扫按 2 秒节流，目录加载在桥接未就绪或失败时按 120 毫秒起步指数退避（上限 2 秒）且同一时刻只保留一个刷新计时器，窗口聚焦重载在上游目录未变化时跳过全量失效投递；慢启动保护的 Statsig 客户端轮询在首秒后从 50 毫秒退避到 250 毫秒。
- 后台会话状态轮询对每个变更的 rollout 采用可续解析：JSONL 只追加时按已消费字节偏移续读并只解析新增行，因此活跃会话不再每 3 秒重读整份历史；首次读取、重写后的全量回退和增量尾部都通过复用行缓冲区流式消费，不再把整份 rollout 读成一个大字符串。缓存只保留一份可续解析 state；文件变化时直接接管旧 state 的所有权，最终聚合时才生成调用方需要的拥有型结果。无 rollout 变化且没有待确认调用时，缓存与 watcher 通过同一个只读 `Arc` 复用上一轮聚合快照；存在待确认时只重建持续时间会变化的 pending 列表，started/aborted/completed 事件、session 状态与 turn 配置继续按各自 `Arc` 复用，不再每轮深复制 5 个 `Vec` 和 1 个 `HashMap`。每个 rollout 只保留最近 256 个终态 turn 及最多 512 份 turn 配置，终态到达时同步清除该 turn 的待确认调用；通知 tracker 的终态去重集合上限与 64 个最近会话的缓存总容量一致，避免长会话轮询导致 Codey 常驻内存与每轮复制成本持续增长。已消费前缀的头尾各 64 字节使用固定内联缓冲区保存并在续读前校验，校验读取不再临时分配 `Vec`；Codey 自身重写 rollout（删除对话轮、归一 provider）或文件被截断时自动回退为全量解析。只读 SQLite 连接会在数据库文件未变化时跨轮询复用，避免稳定空闲期反复打开同一状态库。会话标题/排序缓存的同步锁与 SQLite 工作整体位于 blocking worker 内，async future 不再持锁跨 `await`，同一个 cache 仍按顺序独占复用。活跃任务保持 3 秒检测，稳定空闲时按 3/6/12/30 秒退避，窗口恢复或用户交互会立即唤醒。
- 上游模型目录请求在请求级设置 12 秒总时限，并在读取 chunk 时强制执行 8 MiB 响应上限；解析结果最多接受 10000 个唯一模型，每个模型 ID 最多 512 个 UTF-8 字节。启动同步外层的 5 秒预算继续覆盖源配置解析和整个请求。配置页目录合并使用线性 Set 去重，模型弹窗关闭时不构造内容，打开后支持搜索并按 200 项分批挂载，避免大目录一次创建全部 React 节点。
- 运行期 CDP bridge 将 websocket 读取、handler 执行和响应写回解耦：只读状态、模型目录、账号额度和插件列表最多并发执行 8 项，其他 API、懒加载以及会话导入导出仍进入单一串行通道；待处理队列上限为 256。协议代理入口只解析一次 Responses 请求 JSON，Chat SSE 转换器接管已拥有的请求对象；诊断日志通过 4096 项有界后台队列写入，按 64 条或 100 毫秒批量刷新，队列满时快速失败并在后续日志中记录丢弃数。rollout 头缓存的版本、provider 和条目未变化时不再仅因校验时间变化而重写文件。
- Codex Trace 写盘防护通过 SQLite `block_log_inserts` trigger 阻止 `logs_*.sqlite` 持续写入高频诊断日志；设置开关，已有日志和会话数据不会被删除。
- macOS Crashpad 磁盘保护与 Trace 共用诊断存储界面，但保持独立策略和开关。它只检查 `Application Support/Codex/Crashpad/pending` 与旧版 `Application Support/com.openai.codex/web/Crashpad/pending` 两个 allowlist 目录，不递归搜索其他产品数据；只把 UUID 命名的 `.dmp` 与 `_sidecar.json` 识别为同一报告组，跳过符号链接、未知文件、子目录及 Crashpad 的 `new`、`completed`、`attachments` 和设置文件。保护默认开启：启动时执行一次，此后每 5 分钟检查；总占用超过 512 MiB 时按最旧完整报告组回收到 384 MiB，至少保留最近 10 分钟写入。自动收敛不删除孤儿文件；手动清理可额外删除静默超过 24 小时的已识别孤儿。删除前后复核文件长度、修改时间及 Unix inode/device，消失或发生变化按并发竞争跳过。扫描、部分删除或后台任务失败只进入本地错误日志和诊断快照，不阻断 Codex 启动。
- Windows 默认开启新版卡顿补丁：Codey 在 Codex 主进程执行前通过仅绑定 `127.0.0.1` 的临时 Inspector，把会反复触发原生 DLL 加载失败的 `@worklouder/device-kit-oai` 替换为无设备桩，并精确断路每 30 秒启动一次的 `child-process-snapshot-worker.js`。断路后直接返回合法空快照，不再启动 PowerShell，也不会执行 `Get-CimInstance Win32_Process` 和 `Win32_PerfFormattedData_PerfProc_Process` 两次 CIM/WMI 全量查询；普通 Worker 不受影响。Inspector 随后立即关闭，不修改 Microsoft Store 安装目录。
- macOS / Windows 启动补丁会从 Codex app-server 的本次进程参数中移除 `--analytics-default-enabled`，追加进程级 `analytics.enabled=false` 覆盖，并在主 bundle 中显式关闭桌面主进程与 worker 的 CES 批量遥测，不改写用户配置。补丁同时移除 Codex 每 30 秒向当前 Renderer 拉取完整 app-state、仅写入调试日志与 Sentry breadcrumb 的诊断 heartbeat，并把每次 `browser-window-focus` 触发的外部插件状态检查合并为 30 秒 leading + trailing 节流，减少频繁切换窗口时对 Chrome profile、插件 marketplace 和本地清单的重复扫描；Renderer 就绪或显式触发的诊断快照仍保留，窗口内发生的插件变化仍会在尾部补做一次检查。
- Windows Renderer 会独立注入 Git 请求保护脚本，直接包装 Codex preload 暴露的 `electronBridge.sendWorkerMessageFromView("git", ...)`。若新版 Windows Renderer 中 `electronBridge` 晚于注入脚本出现，或桥接方法以可配置 accessor 形式暴露，状态探针会主动重试并用属性重定义完成接管，避免脚本已执行但控制台长期显示未验证。保护器只识别 `git-origins`、`status-summary`、`review-summary`、`branch-diff-stats` 以及包含这些只读查询的 `subscribe-live-query`；写操作、未知方法、其他 worker 和非 Windows 平台完全透传。首批请求使用容量为 3 的令牌桶通过，持续速率补充为每秒 1 个，同一仓库与查询键至少间隔 2 秒；等待队列总量封顶 48、单键封顶 6，最长等待 15 秒。尚未发送的请求收到原生 cancel 时会从队列移除；传输或可观察的 worker 响应失败会触发最高 15 秒退避。脚本不伪造 Git 结果，也不缓存或合并不同 request ID，避免让 Codex worker 的 pending 请求失去对应响应。
- macOS / Windows 默认开启宠物硬阉割：Codey 先把 Codex 自带的 `electron-avatar-overlay-open` 启动状态设为关闭，再在主进程执行前安装仅存在于本次进程内的断路补丁。补丁在 V8 编译 Codex 主 bundle 前把宠物 manager 构造替换成无状态桩，并拒绝创建 356×320 宠物 BrowserWindow、`Pet Surface`、专用 preload 和 macOS 原生 `avatar-overlay.node`；启动器会等到主 bundle 编译完成并确认 manager 源码已经移除后才报告性能补丁就绪。Windows 确认失败时会先清理暂停实例，再以兼容模式启动，并把本次宠物精简标记为失败、提示可能存在额外 Renderer，下次启动自动重试；macOS 仍会停止启动。BrowserWindow 兜底还会跟踪后续导航，若普通窗口尝试进入 `initialRoute=/avatar-overlay`，会在加载前阻止或立即销毁窗口，并通过只读计数暴露拦截、销毁和清理结果。因此正常路径不会注册宠物生命周期、计时器、原生合成或额外 Renderer。Codex 设置页的 Pets 入口会在激活前按新旧语义 ID 屏蔽，设置 chunk 对 `codex-avatar` 的静态依赖也会替换成无资源桩，避免设置页预先载入宠物 Renderer 和内置精灵图；个人菜单和命令菜单中的宠物控件继续屏蔽。关闭开关后会在下一次由 Codey 启动 Codex 时撤掉断路补丁并恢复宠物及其控件，不改写 `app.asar`。
- macOS / Windows 默认开启语音精简：除旧版听写与全局听写外，也会屏蔽新版 GPT Voice / Realtime Voice 的首页推广、Composer、设置和快捷键入口，并拒绝麦克风设备枚举、音频采集、WebRTC 会话与听写网络连接。关闭开关后会在下一次由 Codey 启动 Codex 时恢复完整语音能力，不改写 `app.asar`。
- 可选的 FastCtx 上下文优化默认关闭。打开后，Codey 会在下次启动 Codex 时优先沿用用户已经配置的 FastCtx；没有现有配置时才把内嵌版本作为本地 STDIO MCP 临时注册，提供带分页和输出预算的 `read`、`grep`、`glob` 与 `replace` 工具，减少文件读取、搜索和机械替换产生的命令拼装与冗余上下文；无需另外安装 FastCtx、npm 包或 Node.js。
- 可选的子代理协作优化默认关闭。打开后，Codey 会在下次启动 Codex 时临时启用 `features.multi_agent_v2`、移除冲突的 V1 `[agents]` 字段、写入官方 `[agents].default_subagent_model` 与 `default_subagent_reasoning_effort`、追加用户级探索委派提示词，并生成不再固定模型与推理强度的 `agents/default.toml`。子代理候选统一采用新版 Codex 的规则：当前线路可用且 `multi_agent_version` 未明确标记为 `disabled` 的官方、第三方和手动模型均可选择，隐藏内部模型不会进入 Codey 的固定目录。为兼容仍要求显式 V2 标记的旧内置 runtime，Codey 会在生成运行时目录时把其中所有可见且未禁用的模型（包括合成的第三方模型）标记为 `v2`；明确 `disabled` 的模型保持不变。`0.147.0-alpha.8` 及以上 runtime 原生支持新版规则，Codey 保留其上游标记；无法解析版本时采用兼容转换，避免重新退化为 Sol/Terra 白名单。runtime 版本来自 Codey 实际解析并启动的 Codex 桌面应用：macOS 执行所选 `.app/Contents/Resources/codex --version`，Windows 执行所选安装目录的 `resources/codex.exe --version`，结果按可执行文件长度与修改时间缓存；不读取 shell 中另装的 Codex CLI，也不使用共享 `models_cache.json.client_version` 作为能力门控。配置页在优化关闭时锁定子代理模型与推理档位；线路切换时后端依次尝试默认模型、线路默认模型和首个动态兼容模型，并优先选择默认档位可用值。启动同步或手动刷新第三方线路模型后会再次执行同一兼容性收敛：当前值不可用时改选首个兼容模型，没有兼容模型时关闭优化。功能已经在当前运行时启用时，保存新的模型或推理档位会通过 Renderer 的 app-server 信号桥调用 `config/batchWrite`，写入两个 `[agents]` 默认值并用 `reloadUserConfig=true` 热重载所有已加载任务；后端只有在 Renderer 确认应用成功且临时配置租约快照同步完成后才推进子代理运行时基线，失败则保留重启标记。正常退出或下次异常恢复时还原启动前内容，运行期间发生的独立用户修改会保守保留。
- Windows 原生 EXE 启动会移除继承到子进程的陈旧 `WSL_DISTRO_NAME`，避免新版客户端无意同步探测 `wsl.exe`；用户在 Codex 中明确启用的 WSL 模式不受影响。
- 配置页提供“清理诊断存储”按钮：同一操作会在线清空 Trace 日志、截断 WAL 并压缩数据库，同时清理已稳定写入的 Crashpad 完整报告组；不会直接删除运行中仍被 Codex 持有的 SQLite 文件，也不触碰会话、账号、配置、插件或 Crashpad allowlist 之外的数据。Trace 与 Crashpad 分别返回清理结果，部分失败不会隐藏另一侧已经完成的回收。
- 诊断存储使用两个独立统计模块和一个组合刷新命令。Trace 快照展示日志条数、SQLite 实际占用和内容字节估算；Crashpad 快照展示目录、完整报告、文件、占用、时间范围和是否超过上限。两个 blocking 扫描并发执行并分别原子替换内存快照；配置页状态查询只序列化现有快照，不触发磁盘扫描。
- 侧边栏相对时间批量查询会话排序键时，只探测一次 `threads` 表的时间列并复用同一条预编译语句，不再为每个会话重复执行 `PRAGMA table_info` 与语句准备。
- 会话与插件修复在每次启动 Codex 前自动执行；普通模式的目标 provider 只读取得 Codex `config.toml` 当前活动值，根键缺失时按 Codex 规则使用内置 `openai`；CC Switch Live 模式只接受同一份已验证路由快照中的 provider。会话修复不会创建、重命名或切换 provider，也不会把悬空或高风险的保留 ID 写入历史。所有 rollout JSONL 的 `session_meta.payload.model_provider` 与全部 Codex SQLite 中的 `threads.model_provider` 会永久对齐到该目标，同时补齐 `has_user_event`、`cwd` 和工作区路径；运行中切换 Live 线路会自动安全重启并重新对齐全部历史。Codey 不在退出时回滚这些会话改动，修复后直接启动原版 Codex 仍能看到历史会话。
- 启动官方 Codex 前会清理 `session_index.jsonl` 中既不存在于 rollout、也没有任何 SQLite 引用的精确格式幽灵任务。索引缺失或没有可清理条目时直接跳过，不再为此遍历全部 rollout 并对每个 Codex 数据库做全表扫描。首次解析会记录精确候选行身份，真正过滤时直接复用该计划，不再为同一 JSONL 做第二轮反序列化；重复 ID、未知结构、损坏行、CRLF 与无末尾换行保持原有语义。写入前保存原始索引并做快照一致性校验，备份位于 `~/.codex/backups_state/provider-sync`，保留最近 5 份 Codey 索引清理备份。
- 新版 Codex 的消息选择按 `data-turn-key` 选择整轮对话，删除前备份 rollout JSONL 并原子替换；旧版 SQLite 消息表继续兼容。
- 每条侧边栏会话提供数据导出按钮，生成带 `Codey会话-` 文件名前缀的可移植 `.codey-session.json`；导出时直接流式转义 JSONL 内容，不再为每行分配第二份转义字符串，并在序列化过程中强制执行 512 MB 传输上限，临时文件不会先膨胀到上限之外。会话列表标题栏兼容 Codex 的 `Tasks` 与 `Recents` 两代分区名称并提供全局导入入口，本地项目目录也提供导入按钮，可恢复完整 rollout 并将会话挂到目标项目。重复 ID 会自动导入为副本，不覆盖已有会话。
- 配置面板提供“恢复备份”，默认恢复最近一次会话数据库备份，也可通过 `restore_session_backup` 命令传入备份目录。
- 官方 curated、embedded remote 和本地工具插件市场通过 CodeyRuntime core 的兼容逻辑注册，页面层合并本地插件并清理隐藏/远程路径字段。
- 配置面板可保存用户脚本；脚本作为独立 CDP 文档脚本在内置修复脚本之后执行。

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

未配置上述 variable 或 secret 时，现有 GitHub Release 发布不受影响，R2 同步会被跳过。默认构建使用项目公开的 R2 更新源；设置 `CODEY_UPDATE_BASE_URL` 可以在编译时覆盖该地址。配置页面不允许用户改写更新源。检查更新会经 HTTPS 拉取清单，校验版本、下载地址和 SHA-256 格式后显示是否有新版本。当前 macOS 包仍是未签名包，Windows 包也尚未进行代码签名，因此检查更新不会自动下载或静默安装。

Codey 将运行时 core/data crate 固定在 `vendor/CodeyRuntime`，生命周期、会话扫描优化以及显式配置的独立协议代理句柄也已直接合并其中。主程序只复用该句柄和既有 Responses↔Chat 转换器，不接管 vendor 的整套启动器或全局设置。后端启动编排与 macOS/Windows/Unix 进程适配分层维护，运行时 TOML 三方恢复算法和私有原子文件 I/O 基元也已与 provider 应用/租约编排分离。本地与 CI 构建不需要额外的运行时源码目录或补丁。这些 crate 与后端同属根 Cargo workspace，`cargo test --workspace` 一条命令覆盖全部；统一依赖解析与特性合并消除了两个独立 workspace 的重复编译。PR 质量门在 Linux 上执行格式检查、完整测试及零警告 Clippy，Windows CI 补充该平台测试与 Clippy；桌面发布构建只保留 macOS 的 Rust 测试（macOS 无独立 CI 任务），Windows 的 Rust 检查由 CI 门保证，打包流程不再重复编译。

运行时只内置不含提示词的 Codex 模型兼容元数据，完整 system/developer prompt 不进入仓库资产或 CodeyRuntime 二进制。Codex 当前要求自定义模型目录的每个条目都保留 `base_instructions`；因此 Codey 只从用户本机已有的官方 `models_cache.json` 派生运行目录，原样保留本机缓存中的必需字段，并把生成文件权限收紧为仅当前用户可读写。缺少兼容的本机缓存时不生成不完整目录，官方线路回退 Codex 内置目录，第三方线路仍可完成上游模型探测、手动模型选择保存与子代理能力校验；这是可恢复的内置目录回退，不记录为补丁失败。模型选择保存与线路模型同步必须只吞掉该明确的缓存兼容错误，目录读写或解析错误仍应返回给用户。这类本机派生内容不得写入日志、测试夹具、发布包或版本库。

## 配置与路径

- Codey 配置：由 `directories` 根据系统保存到 Codey 配置目录下的 `config.json`。
- cc-switch 配置：数据库用于判断 CC Switch 路由接管状态、匹配直连线路的协议提示，以及在有效路由接管下为模型目录请求只读解析当前源 API；不会替代 Codex 配置成为活动线路来源，也不会持久化源凭据。`CC_SWITCH_DB_PATH` 指定的数据库文件优先级最高；否则读取 cc-switch Tauri Store 中的 `app_config_dir_override` 并跟随其自定义数据目录，未配置覆盖时使用 `~/.cc-switch/cc-switch.db`。Windows 还与 cc-switch 一致：仅在默认数据库不存在时兼容旧版 `HOME/.cc-switch/cc-switch.db`。
- Codex 配置：使用 Codex 默认 `CODEX_HOME`（通常是 `~/.codex`）。
- Trace 写盘防护由 `disableTraceLogWrites` 控制，默认开启；macOS / Windows 使用相同启动时机更新 Codex 根目录及旧版 `sqlite/` 目录中现有的 `logs_*.sqlite`，不会创建、清空或压缩日志库。macOS Crashpad 容量保护由独立的 `protectCrashpadPending` 控制，默认开启且保存后热切换；Windows 保留兼容配置字段但不扫描 Crashpad 目录。
- Windows 卡顿补丁不设开关：Codey 在运行时识别 Windows，并在每次启动 Codex 时自动隔离 Micro 设备模块和周期性 WMI 进程采样。启动 Codey 时若目标 Codex 主进程已在运行，会先终止该安装目录下的 Codex 进程树，确认退出后再拉起新主进程，确保补丁能在主进程执行前安装；清理失败会中止启动。macOS 不执行 Windows 专属分支。
- 宠物硬阉割：`slimCodexPet` 默认为 `false`。为兼容需要 Avatar Overlay 控制器的新 Codex 启动流程，读取到旧配置中的 `true` 时会自动迁移为 `false`；关闭后下次启动会恢复完整宠物功能。
- 语音精简：`slimCodexVoice` 默认为 `false`，macOS / Windows 都会在下次通过 Codey 启动 Codex 时生效。开启后同时覆盖旧听写和新版 GPT Voice / Realtime Voice；关闭时保留完整语音功能。
- 浮动额度：`showAccountUsageInHeader` 默认为 `true`，保存后立即生效且不要求重启。只有活动线路被识别为官方账号登录时才请求并展示，切到第三方线路后保留开关值但停止请求和显示；用户手动关闭后的持久化值不会被默认值覆盖。
- Codex 慢启动保护：`fastCodexStartup` 默认为 `true`。Codey 会在 Electron 主进程仍处于启动暂停阶段时，为登录后的 Statsig bootstrap 设置 1.5 秒上限，并保留 renderer 保护作为兼容兜底；正常响应保持原流程，慢请求或失败请求会让 Codex 使用自身错误降级路径继续挂载主界面。原始初始化仍可在后续刷新中恢复；关闭后下次启动完全使用 Codex 原生等待策略。
- FastCtx 上下文工具：`fastContextTools` 默认为 `false`。打开后下次启动 Codex 生效；应用临时配置前会检查 `mcp_servers`，只要已有 server 的 ID、`command` 或 `args` 以独立 token 命中 `fastctx`（大小写不敏感），就完整保留并复用现有配置，不再注册 `codey_fastctx`，也不追加 Codey 专属 namespace、输出预算或工具指引。未检测到时，Codey 才在本次运行的临时 `config.toml` 中注册随 Codey 分发的 `codey-fastctx` sidecar 作为独立的本地 STDIO MCP（FastCtx 及其 o200k 分词器数据只编入该 sidecar，不再进入 Codey 主程序；主程序仍会把旧临时配置里的 `--codey-fastctx-mcp` 调用代理到 sidecar，sidecar 缺失时本次启动按未启用 FastCtx 继续并记录错误日志）、设置 FastCtx 自身的 8500 token 预算，并在用户没有配置 Codex 工具输出上限时设为 10000 token，随后追加工具使用指引；退出时随 provider 配置一起恢复原文件。关闭开关时还会幂等清理带 `--codey-fastctx-mcp` 标记的内置 server、作为完整段落出现的新旧两版 Codey FastCtx 固定提示词，以及能与这些 Codey 自有项共同确认的 `mcp__codey_fastctx` namespace；用户自己的 server、无法证明归属的 namespace、输出上限和其他提示词保持不变。FastCtx 通过 MCP tools 接口调用，工具名由 server ID 决定，例如 `fastctx` 对应 `mcp__fastctx__read`；文件路径传绝对路径，`resources/read` 属于另一套 MCP 接口。
- 子代理协作优化：`subagentOptimization` 默认为 `false`。关闭时配置页仍允许预先选择子代理模型或推理档位，但不注入运行时协作优化；设置页展示所有未明确禁用模型与当前线路已知支持范围的交集，当前集合为空时禁止开启并解释兼容限制。切换线路以及启动或手动刷新线路模型目录时都会重算可用性，已保存的子代理模型仍兼容当前线路时优先保留，当前模型不可用时才回退到当前目录首个兼容可用模型；新模型仍支持已保存推理档位时保留用户选择，仅在不兼容时回退模型默认档位；没有兼容模型时同时关闭优化。开启前会校验当前线路是否支持当前子代理模型；第三方线路会实时刷新上游模型列表，不支持或无法确认时保持关闭并提示。模型 ID 在配置归一化、热更新快照和线路覆盖层中只做去空白与非空校验，不再被静态名称列表改写；实际资格统一由 `ModelSelectionState.subagent_model_ids` 判定。打开后下次启动 Codex 生效；`config.toml`、`AGENTS.md` 与 `agents/default.toml` 的变更纳入同一个运行时租约，非法档位仍会在覆盖层重应用时归一化，退出时自动恢复。`config.toml` 使用三方合并回滚 Codey 拥有的字段，提示词只移除 Codey 注入的完整块，用户运行期间替换过的 `default.toml` 不会被覆盖。
- 子代理协作优化开启时，Codey 同时关闭 `features.multi_agent_v2.expose_spawn_agent_model_overrides`，让 `spawn_agent` 工具不再暴露显式模型与推理档位覆盖参数；子代理统一使用 `[agents]` 中由 Codey 写入并热重载的默认值，避免空字符串覆盖绕过默认配置并触发 runtime 参数解析错误。
- Codex App 路径：可在 Codey 配置界面填写；留空时使用 CodeyRuntime 的平台发现逻辑。Windows 自动发现失败或已保存路径失效时，会在启动阶段打开原生目录选择器并持久化规范化后的应用目录，因此自定义盘符不依赖尚未启动的 Codex 页面；目录解析兼容安装根目录下的 `app`、`bin`、`current` 与 `versions/current` 布局。
- CDP 默认端口：`9229`，如 Windows 端口被占用会按 core 的逻辑选择可用回环端口。

### 通知渠道扩展

通知实现按“公共调度 + 渠道适配”拆分。后端 `backend/src/notifications/` 中的配置、事件、格式化和调度器不依赖具体渠道；每个发送渠道放在 `channels/` 的独立文件中，实现 `NotificationChannelAdapter`，并在 `channels/mod.rs` 注册。新增渠道时需要同时补齐渠道枚举与配置字段、请求构造、明确的成功响应校验、传输与响应错误脱敏及对应单元测试；HTTP 成功但响应损坏或缺少渠道成功字段仍按发送失败处理。

前端 `src/notifications/` 以 `channelRegistry.tsx` 为唯一渠道注册入口，每个渠道使用独立编辑器组件；注册项负责显示信息、默认配置和完整性判断，公共列表只负责展示、编辑和删除，启用状态与测试发送都在渠道编辑弹窗内配置。新增和编辑必须先完成渠道配置，并经不落盘的 `test_notification_channel` 测试成功后才能保存；每次修改草稿都会要求重新测试。外部配置结构继续使用 `webhook.channels`，既有 `test_webhook` 仍保留以兼容已有渲染层调用和持久化数据。涉及凭据的渠道必须保持普通配置返回渲染层前脱敏、留空保存时回填旧值、显式清除时不回填；仅在用户主动打开某一渠道编辑弹窗时，可经 `reveal_notification_channel` 按需返回该渠道凭据，弹窗关闭后立即清空本地草稿。

## 启动与恢复

打开 Codey 后不会创建常驻原生配置窗口；仅当 Windows 无法解析 Codex 应用路径时，启动阶段会显示一次系统目录选择器。Codey 会先恢复上次租约并同步当前线路；CC Switch Live 模式随后建立并校验不可混配的路由快照，普通模式则只读取得 Codex 当前活动 provider。只有目标 provider 验证完成后才永久同步 rollout 与 SQLite、清理幽灵任务索引，接着备份并临时应用运行时配置、修复插件市场、启动 Codex，最后通过 CDP 注入轻量控制脚本；会话修复本身不会改写活动 provider。Windows 和 macOS 启动时会按目标主可执行文件判断 Codex 是否正在运行；命中后先终止同一安装目录下的主进程、Helper、app-server 及后代进程树，确认退出后再由 Codey 拉起，清理失败则中止启动。首次 Codex 启动失败时，Codey 会调用与正常退出相同的运行时停止和配置恢复逻辑，失败后等待 100 毫秒重试一次；Windows 随后通过阻塞任务显示原生错误对话框，用户关闭对话框后当前 Codey 进程返回错误并退出，不进入常驻关闭等待。首次点击 Codex header 中的 “Codey” 按钮时才会加载紧凑 React 浮层，配置操作通过本次 CDP bridge 发送给 Rust 进程。遮罩空白处、右上角关闭按钮和 `Esc` 都能关闭浮层。关闭这次由 Codey 拉起的 Codex 后，Codey 会先标记退出、取消并等待尚未执行完的延迟重启任务，再停止路由 watcher，终止该 Codex 的主进程、Helper、app-server 及后代进程树，恢复临时配置，最后清理其他遗留 Codey 进程并自行退出；收到系统退出信号和安装更新时也执行同一套清理。遗留 Codey 清理只接受与当前程序完整路径一致的首次进程快照，并在每次终止前复核 PID 的启动身份；轮询期间不会吸收新进程，避免同名程序或 PID 复用导致误杀。会话 JSONL、数据库与索引清理结果不回滚。若 CDP 注入失败，Codey 会停止本次启动、显示原始错误并退出，不会另起本地 Web 服务。

Codey 不改写 `auth.json`，因此 Codex 的账号栏仍会显示原来的官方登录账号；这只代表客户端登录会话，不代表第三方 provider 仍走官方接口。读取 Codex 活动线路时，provider 范围内的 `experimental_bearer_token` 优先于 `auth.json` 中的 API Key。非路由模式运行期间当前 provider ID 保持不变，第三方 API 地址、协议和 bearer token 会直接写入该 provider 的临时配置；内置 `openai` 官方线路继续使用 Codex 自身的 provider 定义，不写入无效的保留 ID 覆盖。第三方线路若错误使用 Codex 保留 provider ID 会在启动前被拒绝并提示改用非保留自定义 ID；路由模式则完整保留 CC Switch Live provider 表与接管 token。

如果 Codey 异常退出，下次启动前会检查 `codex-lease.json`；非路由租约会在 provider 仍保持上次由 Codey 应用的 API 地址时先恢复备份，再应用当前线路。路由租约不以 provider ID 或地址作为恢复前提，而是对启动前原始内容、Codey 已应用内容和当前 CC Switch 内容做三方合并，只撤销 Codey-owned 字段，避免 CC Switch 切换 provider 后因保护性早退而遗留 overlay。缺少已应用快照的旧租约只回滚旧版本明确拥有的 provider、模型目录、推理档位与 FastCtx 字段，并保留插件、市场、用户新增键及同表中的并发扩展，不再整文件覆盖或删除当前配置。非路由模式若用户在 Codey 运行期间手动改写了 provider 或地址，恢复逻辑会保守地不覆盖该修改。Codey 自身的所有 `config.json` 读改写事务共用一把异步写锁；整份设置保存还携带持久化的 `settingsRevision`，旧页面或并发请求提交的过期快照会被拒绝，避免静默覆盖已经完成的配置更新。启动备份目录采用保留策略：应用运行时配置前清理 `codex-backups` 下最旧的启动备份，保留最近 5 份及当前租约引用的目录。

## 已知限制

- 目标是 Codex Electron 桌面客户端，不覆盖 CLI。
- Windows 新版卡顿补丁针对 Codex Micro / Work Louder 设备集成导致的原生模块异常，以及当前客户端的周期性 WMI 遥测采样；Windows 上会自动启用，不会连接 Codex Micro 硬件，也不会启动该遥测 Worker 或 PowerShell。插件 app-server 在清理旧进程时可能执行的一次性 WMI 查询仍保留，避免产生孤儿进程；它不是 30 秒反复调用的来源。Git 请求保护只覆盖经过当前 Renderer worker bridge 发出的目标请求，无法拦截 Git worker 或原生 app-server 已经接受订阅后在内部自行触发的刷新，因此它是降低请求风暴速率的前置保护，不是 Windows 内核资源异常的完整修复。宠物硬阉割与 FastCtx 上下文工具保留用户开关。
- 当前 Codex 优先按 `threads.rollout_path` 定位 JSONL，并按 `task_started.turn_id` 删除整轮记录；旧版 `messages`、`thread_items`、`items` SQLite schema 作为兼容路径。
- 内嵌 FastCtx 当前只发布文件读取、搜索、发现与批量替换工具，不发布 MCP Resources 接口及其可选 Bash/后台任务组。Codey 注入的规则会把 URI 形态的本地引用先规范化为普通绝对路径，再直接交给 FastCtx read 工具；PDF 引擎未编入 Codey，PDF 应继续使用 Codex 自带的 PDF 能力。
- 第三方线路可以提供 Codex 原生支持的 Responses API，也可以提供 OpenAI 兼容的 Chat Completions API；后者由 Codey 在运行期间通过临时回环代理完成 Responses↔Chat Completions 转换。原生 Anthropic、Gemini 等其他协议不在适配范围内。
- 页面注入使用稳定的 `data-*`/`electronBridge.sendMessageFromView` 探测，Codex bundle 大幅改版时可能需要更新选择器适配层。
- 消息通知按渠道列表保存，支持同时配置多个飞书 Webhook 与 Telegram Bot；旧版单飞书配置在读取时自动迁移。飞书接受官方或企业内网主机名的 HTTPS 机器人地址，仍要求 443 端口、标准 `/open-apis/bot/v2/hook/...` 路径且禁止 URL 用户信息、查询参数和片段；通知专用 HTTP 客户端不跟随重定向。`session.completed` 由真实 Codex turn 的完成状态触发，不再把单次模型 HTTP 响应误判为任务结束；失败、等待介入与手动测试仍保留。自动通知会并发投递到所有已启用且配置完整的渠道，并汇总失败；只有连接拒绝或渠道明确返回失败等确定结果才会自动重试，HTTP 超时、响应读取中断及其他没有明确失败响应的传输错误一律视为远端可能已经接收，停止重试并保留本次去重记录。等待介入通知采用写前持久化去重：先原子记录预留再请求渠道，确定失败时回滚；因为飞书与 Telegram Webhook 都没有可依赖的幂等键，进程在预留后、确认响应前崩溃时会保守地抑制重发，边界为 at-most-once。waiting 去重台账按插入序持久化并封顶 2048 条，超出时淘汰最旧键；台账写盘在阻塞线程执行且不占用状态锁。完成/失败通知使用当前进程内的有界去重历史，不承诺跨进程 exactly-once。飞书不保存或发送签名密钥；飞书 Webhook 地址与 Telegram Bot Token 默认不会返回渲染层，并通过配置状态保留已有凭据。用户主动打开单一渠道编辑弹窗时，后端才会临时回显该渠道凭据，弹窗关闭即清空本地草稿。所有通知消息都不包含 prompt、正文、内部会话 ID、线路 ID 或 API Key。
- 首版明文 API Key、飞书 Webhook 地址与 Telegram Bot Token 仅依赖配置文件权限保护，后续可把 `ConfigStore` 的 secret 存取替换为 macOS Keychain/Windows Credential Manager。

FastCtx 集成基于 [yc-duan/fastctx](https://github.com/yc-duan/fastctx) `0.2.4` 的固定提交 `86dac0c`（Apache-2.0）。
