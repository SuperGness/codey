# Codey 内部开发文档

本文档面向 Codey 的开发和维护，保留实现细节、构建发布流程、配置路径、启动恢复机制和已知限制。面向使用者的功能介绍只维护在 `README.md`；不要把协议、端口、路径、构建命令、数据库结构、补丁策略或其他内部技术细节迁回公开 README。

Codey 是一个无界面的 Rust 桌面辅助进程，通过 CDP 连接官方 Codex Electron 客户端，并把 React 配置控制台直接注入 Codex 页面内的隔离浮层。Codey 不监听本地 HTTP 端口；官方线路直接连接 ChatGPT Codex 后端，第三方线路则在 Codex 运行期间直接写入临时 provider 配置，退出后原子恢复启动前的配置。

## 当前能力

- 打开 Codey 时自动启动 Codex，并通过 CDP 注入 Codey 设置按钮、Fast 模式展示修复、插件市场修复和消息选择工具；设置按钮在 Codex 客户端内部打开 Shadow DOM 隔离的 Semi Modal 配置浮层，不跳转外部浏览器。
- Windows 原生 EXE 使用 GUI 子系统，运行期间不会创建命令行窗口。首次启动 Codex 失败时，Codey 会恢复临时配置、显示系统错误对话框并退出；清理失败时，对话框和诊断日志会同时保留启动错误与清理错误。
- 线路采用自动双模式：检测到 `~/.cc-switch/cc-switch.db` 时只读同步当前 Codex provider；没有 cc-switch 时直接读取本地 Codex 活动 provider，兼容用户手工维护的第三方地址、provider token、`auth.json` API Key 及 provider 自身的环境变量和请求扩展字段。同步兼容 cc-switch 的“保留官方登录”模式：第三方 token 可以来自当前 provider 的 `experimental_bearer_token`，`auth.json` 中同时存在的 ChatGPT OAuth 只作为保留账号状态，不得把明确的第三方地址误判为官方线路。若来源 provider 以精确的 `name = "OpenAI"` 声明支持 Codex 远程压缩，Codey 会把该能力随线路配置持久化并在临时 provider 中保留；线路显示名仍使用 CC Switch 中的名称。线路变化需要重启由 Codey 启动的 Codex 后生效。
- 官方线路沿用 ChatGPT 登录；第三方线路把 API 地址和临时 bearer token 直接交给 Codex，并固定使用当前客户端唯一支持的 Responses API，不经过 Codey 转发或协议转换。历史配置或 CC Switch 线路若仍声明 Chat Completions，Codey 会在写入运行时配置前明确拒绝并提示迁移。
- 官方账号线路默认开启浮动额度展示。额度组件以固定定位浮窗挂在 Codex 右下角，默认保留 24px 边距，套餐、周期额度、余额和本地刷新时间纵向展示；拖拽结束后把浮窗 `left/top` 保存在 Codex renderer 的 localStorage 中，并在窗口尺寸变化时约束回可视范围。轻量 renderer 每 60 秒通过 Codey bridge 请求一次额度快照；Rust 后端只在当前 provider 判定为官方且 `showAccountUsageInHeader` 已开启时读取 `auth.json` 的 ChatGPT access token 和 account ID，请求 ChatGPT backend 的 `/wham/usage`，并兼容 `/api/codex/usage` 旧路径。渲染层只接收已归一化的周期、使用比例、重置时间、方案和余额，不接收 OAuth 凭据；第三方线路、关闭开关或请求失败时会自动隐藏组件或保留上一次成功结果并标记为过期。
- CC Switch 路由接管通过数据库 `proxy_config.enabled`、Codex 的 `proxy_live_backup` 及旧版 `proxy_takeover_codex` 设置识别管理态，并用活动 provider 的 `PROXY_MANAGED` 标记或 `cc-switch-official` 回环地址验证 Live 接管态。管理态存在但 Live 标记缺失时，Codey 在改写 provider 之前停止启动并提示用户关闭后重新开启路由。有效接管下跳过全局 provider 迁移、Codey 模型目录刷新以及 `model_provider`、`model_providers`、`model`、`model_catalog_json` 写入，只叠加推理档位、FastCtx、子代理等 Codey 自有运行时字段。
- 配置页以官方账号可见的 7 个模型为固定左列；每次拉起第三方线路前会在 5 秒上限内直接请求当前 provider 的 `/v1/models` 或 `/models`，同步成功后仅向 Codex 展示上游支持的模型，无需再手动同步并重启。请求失败、超时或返回空列表时优先沿用该线路上次保存的模型支持配置，首次使用且尚无保存配置时才回退到固定 7 模型并继续启动。配置页手动同步失败后仍会打开模型弹框，明确提示线路可能不支持模型目录接口；弹框始终列出 7 个官方模型供用户勾选，并允许输入其他模型 ID。其他模型输入会在前后端同时拒绝官方清单中的模型，保存时把官方勾选与其他模型共同写为该线路已确认的支持范围。模型支持范围、上游目录或默认模型保存后，后端会通过当前 renderer 的 CDP 连接把新目录直接传给模型白名单 `setCatalog()`，避免保存请求内部再次调用 bridge 形成重入等待；renderer 同时改写 Statsig 模型配置、触发 `values_updated`、刷新 React Query 的 `models/list` 活跃缓存，并在 app-server 返回旧目录时于消息捕获阶段替换模型描述。后端除校验 `snapshot()` 的模型顺序与默认模型外，还要求命中 Statsig 订阅和当前模型查询缓存才把本次保存报告为立即生效；运行时模型基线仅在这些校验成功后更新，因此模型变更可单独清除重启标记，热刷新失败时则保留重启要求。
- 启动前备份 Codex `config.toml`，退出时按 lease marker 原子恢复，`auth.json` 和官方登录状态保持不变。CC Switch 路由模式会每秒检查一次 Live 配置；外部内容稳定两个检查周期后，用原始、已应用和当前内容剥离旧 Codey overlay，再基于最新 CC Switch 基线重加 overlay，并滚动更新租约快照。退出恢复前会先停止并等待该 watcher，因此热切换后的 provider、模型和目录会保留，Codey 自有字段仍能被准确撤销。
- 启动器对 `sessions` 与 `archived_sessions` 的 rollout 采用逐行流式检查；只有确实需要改写 provider 的文件才会载入全文，避免长会话历史在启动时形成多份大字符串并把内存峰值长期留在分配器中。
- 启动器只读取 rollout 的首个 `session_meta` 头并流式遍历目录，不再为校验构建全量路径列表；头部校验按目录分片到最多 4 条线程并发执行，任一目录发现 provider 不匹配即整体提前结束。Trace 防护、插件维护和宠物状态会在依赖关系允许时并行执行，一次性日志统计则在 Codex 可用后后台完成。官方模型目录在同一次启动内按文件大小和修改时间复用解析结果，不再为 `refresh_for_provider` 和 `selection_state` 各解析一遍。
- Codey 的受控基础脚本会预构建为单个 CDP 文档注入包并在健康恢复时复用，默认注入从 16 次脚本往返降为 2 次；约 689 KB 的 React 设置浮层、按需组件样式与主题变量只在首次点击 Codey 按钮时注入，用户脚本仍保持独立且最后执行。`public/` 注入脚本在 `vite:build` 阶段压缩到 `dist-overlay/inject/` 后才嵌入二进制（esbuild 会把 `__CODEY_*__` 占位符比较常量折叠掉，构建脚本逐文件校验占位符幸存，丢失即回退为源码拷贝）；浮层 CSS 会剔除所有逗号选择器都带 `-rtl` 类的独立规则，与 `body`/`:host` 共享选择器列表的主题变量块保持原样。额度组件在数值未变化时跳过 DOM 重建；CDP 注入重试采用 ~15 秒总预算内的指数退避；每 60 秒的额度刷新会记住上次成功的接口端点，失败时仍回退完整列表。
- 试验性功能补丁在覆盖用户值前保存 Codex 原生 resolver 的完整布尔快照。“同步官方配置”优先读取仍存在的 Statsig gate/layer；gate 下线或转为稳定默认值后，回退到这份覆盖前快照，不再把缺失 gate 误判为 `false` 并覆盖远程压缩等现有能力。
- `codey-errors.log` 继续只记录失败，但所有新增启动诊断统一携带 `stage`、`durationMs`、`attempts`、`timeoutMs` 和 `recoverable` 字段；旧版主进程补丁 helper 记录仍可兼容读取。CDP 注入使用约 15 秒硬 deadline，失败时写入真实重试次数和耗时，不再使用固定估算。轻量 Renderer 在 Electron bridge 5 秒未就绪、Codex header/sidebar 20 秒仍未挂载、语言设置同步或会话工具按需加载失败时，通过受限的 `/diagnostics/error` bridge 路由写入同一份当日错误日志；后端只接受固定 operation，并自行生成错误文本和阶段，只保留白名单状态字段，不接收 Renderer 原始 message、stack、URL 或页面内容。
- Renderer 启动时只保留设置按钮与轻量侧边栏探测；导入、导出、删除、相对时间和消息选择等会话工具要等用户首次悬停、点击或键盘聚焦侧边栏后才加载，加载完成后会撤掉启动探测观察器。增量观察器按新增控件最近的会话行、项目行、侧边栏分区或消息轮次修复，刷新前再次合并祖先/后代根节点，且仅在顶栏确实变化时重找设置按钮；节流在持续变更下最多推迟 250 毫秒，避免流式输出把刷新无限期饿死。命中消息轮次的根节点只跑消息选择安装器，不再对整轮子树重复执行侧边栏安装器；会话 ID 探测只在用户真正硬删过消息后才进行，消息选择按钮按行缓存而非每次全子树查找。相对时间只遍历已登记的会话行并跳过无变化的 DOM 写入，窗口回前台触发的强制刷新按 10 秒去抖。观察器不监听流式正文的 `characterData`；插件 bridge 使用有界指数退避等待宿主接口，也不会再序列化无关 IPC 的完整参数，并在解析请求体前先做子串预筛，避免为无关请求整体 `JSON.parse`。
- 宠物与语音屏蔽脚本的 React fiber 判定按元素缓存，宠物盾的文档观察器改为 50 毫秒尾部节流并合并祖先/后代根节点，被观察属性变化时才失效对应缓存；完全访问权限提示屏蔽只扫描新插入的子树并改用 `textContent`，不再每次触发整页按钮遍历和布局刷新。模型白名单的交互重扫按 2 秒节流，目录加载在桥接未就绪或失败时按 120 毫秒起步指数退避（上限 2 秒）且同一时刻只保留一个刷新计时器，窗口聚焦重载在上游目录未变化时跳过全量失效投递；慢启动保护的 Statsig 客户端轮询在首秒后从 50 毫秒退避到 250 毫秒。
- 后台会话状态轮询对每个变更的 rollout 采用可续解析：JSONL 只追加时按已消费字节偏移续读并只解析新增行，因此活跃会话不再每 3 秒重读整份历史。已消费前缀的尾部字节会在续读前校验，Codey 自身重写 rollout（删除对话轮、归一 provider）或文件被截断时自动回退为全量解析。只读 SQLite 连接会在数据库文件未变化时跨轮询复用，避免稳定空闲期反复打开同一状态库。活跃任务保持 3 秒检测，稳定空闲时按 3/6/12/30 秒退避，窗口恢复或用户交互会立即唤醒。
- Codex Trace 写盘防护通过 SQLite `block_log_inserts` trigger 阻止 `logs_*.sqlite` 持续写入高频诊断日志；设置开关，已有日志和会话数据不会被删除。
- Windows 默认开启新版卡顿补丁：Codey 在 Codex 主进程执行前通过仅绑定 `127.0.0.1` 的临时 Inspector，把会反复触发原生 DLL 加载失败的 `@worklouder/device-kit-oai` 替换为无设备桩，并精确断路每 30 秒启动一次的 `child-process-snapshot-worker.js`。断路后直接返回合法空快照，不再启动 PowerShell，也不会执行 `Get-CimInstance Win32_Process` 和 `Win32_PerfFormattedData_PerfProc_Process` 两次 CIM/WMI 全量查询；普通 Worker 不受影响。Inspector 随后立即关闭，不修改 Microsoft Store 安装目录。
- macOS / Windows 启动补丁会从 Codex app-server 的本次进程参数中移除 `--analytics-default-enabled`，追加进程级 `analytics.enabled=false` 覆盖，并在主 bundle 中显式关闭桌面主进程与 worker 的 CES 批量遥测，不改写用户配置。补丁同时移除 Codex 每 30 秒向当前 Renderer 拉取完整 app-state、仅写入调试日志与 Sentry breadcrumb 的诊断 heartbeat，并把每次 `browser-window-focus` 触发的外部插件状态检查合并为 30 秒 leading + trailing 节流，减少频繁切换窗口时对 Chrome profile、插件 marketplace 和本地清单的重复扫描；Renderer 就绪或显式触发的诊断快照仍保留，窗口内发生的插件变化仍会在尾部补做一次检查。
- macOS / Windows 默认开启宠物硬阉割：Codey 先把 Codex 自带的 `electron-avatar-overlay-open` 启动状态设为关闭，再在主进程执行前安装仅存在于本次进程内的断路补丁。补丁在 V8 编译 Codex 主 bundle 前把宠物 manager 构造替换成无状态桩，并拒绝创建 356×320 宠物 BrowserWindow、`Pet Surface`、专用 preload 和 macOS 原生 `avatar-overlay.node`；因此不会注册宠物生命周期、计时器、原生合成或额外 Renderer。Codex 设置页的 Pets 入口会在激活前按新旧语义 ID 屏蔽，设置 chunk 对 `codex-avatar` 的静态依赖也会替换成无资源桩，避免设置页预先载入宠物 Renderer 和内置精灵图；个人菜单和命令菜单中的宠物控件继续屏蔽。关闭开关后会在下一次由 Codey 启动 Codex 时撤掉断路补丁并恢复宠物及其控件，不改写 `app.asar`。
- macOS / Windows 默认开启语音精简：除旧版听写与全局听写外，也会屏蔽新版 GPT Voice / Realtime Voice 的首页推广、Composer、设置和快捷键入口，并拒绝麦克风设备枚举、音频采集、WebRTC 会话与听写网络连接。关闭开关后会在下一次由 Codey 启动 Codex 时恢复完整语音能力，不改写 `app.asar`。
- 可选的 FastCtx 上下文优化默认关闭。打开后，Codey 会在下次启动 Codex 时优先沿用用户已经配置的 FastCtx；没有现有配置时才把内嵌版本作为本地 STDIO MCP 临时注册，提供带分页和输出预算的 `read`、`grep`、`glob` 与 `replace` 工具，减少文件读取、搜索和机械替换产生的命令拼装与冗余上下文；无需另外安装 FastCtx、npm 包或 Node.js。
- 可选的子代理协作优化默认关闭。打开后，Codey 会在下次启动 Codex 时临时启用 `features.multi_agent_v2`、移除冲突的 V1 `[agents]` 字段、写入官方 `[agents].default_subagent_model` 与 `default_subagent_reasoning_effort`、追加用户级探索委派提示词，并生成不再固定模型与推理强度的 `agents/default.toml`；模型来源限定为当前模型选择器已启用的官方或第三方模型，官方模型的推理档位直接来自本机模型目录。配置页在优化关闭时锁定子代理模型与推理档位；线路切换时后端先重置为 `DEFAULT_SUBAGENT_MODEL` 与 `DEFAULT_SUBAGENT_REASONING_EFFORT`，若新线路已知支持范围不包含默认模型，则复用模型目录 `selection_state` 的首个可用模型，并优先选择默认档位可用值。功能已经在当前运行时启用时，保存新的模型或推理档位会通过 Renderer 的 app-server 信号桥调用 `config/batchWrite`，写入两个 `[agents]` 默认值并用 `reloadUserConfig=true` 热重载所有已加载任务；后端只有在 Renderer 确认应用成功且临时配置租约快照同步完成后才推进子代理运行时基线，失败则保留重启标记。正常退出或下次异常恢复时还原启动前内容，运行期间发生的独立用户修改会保守保留。
- Windows 原生 EXE 启动会移除继承到子进程的陈旧 `WSL_DISTRO_NAME`，避免新版客户端无意同步探测 `wsl.exe`；用户在 Codex 中明确启用的 WSL 模式不受影响。
- 配置页提供“清理日志库”按钮：在线清空诊断日志、截断 WAL 并压缩数据库以回收磁盘空间，不直接删除运行中仍被 Codex 持有的文件，也不触碰会话、账号、配置或插件数据。
- Trace 功能使用独立统计模块；Codex 可用后在后台读取一次日志库并原子替换内存快照，仅展示日志条数、SQLite 实际占用和内容字节估算。每个日志库只执行一次汇总扫描，不再额外计算近 7 天走势、级别分布、高占用 target 或 SSD 寿命影响；配置页刷新和状态查询不会再次扫描日志库。
- 侧边栏相对时间批量查询会话排序键时，只探测一次 `threads` 表的时间列并复用同一条预编译语句，不再为每个会话重复执行 `PRAGMA table_info` 与语句准备。
- 会话与插件修复在每次启动 Codex 前自动执行；所有 rollout JSONL 的 `session_meta.payload.model_provider` 与全部 Codex SQLite 中的 `threads.model_provider` 会永久归一到非保留全局 ID `codey_global`（已有自定义 provider 时沿用原 ID），同时补齐 `has_user_event`、`cwd` 和工作区路径。Codey 不在退出时回滚这些改动，修复后直接启动原版 Codex 仍能看到历史会话。
- 启动官方 Codex 前会清理 `session_index.jsonl` 中既不存在于 rollout、也没有任何 SQLite 引用的精确格式幽灵任务。索引缺失或没有可清理条目时直接跳过，不再为此遍历全部 rollout 并对每个 Codex 数据库做全表扫描。写入前保存原始索引并做快照一致性校验，备份位于 `~/.codex/backups_state/provider-sync`，保留最近 5 份 Codey 索引清理备份。
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

Windows 上执行 `npm run dev` 时，脚本只检查本次 Cargo profile 对应的本地 `codey.exe`。发现旧进程会先停止启动并要求从系统托盘或原终端正常退出，以便 Codey 清理 Codex 子进程和临时配置；只有确认进程卡死时才设置 `CODEY_DEV_FORCE_KILL=1` 重试。强制终止后会重新确认该进程已退出，确认失败时不会启动 Cargo；确认无占用后直接执行 `cargo run`。

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

Codey 将运行时 core/data crate 固定在 `vendor/CodeyRuntime`，生命周期和会话扫描优化也已直接合并其中。本地与 CI 构建不需要额外的运行时源码目录或补丁。PR 与桌面发布质量门会分别对根 workspace 和 CodeyRuntime workspace 执行格式检查、完整测试及零警告 Clippy。

运行时只内置不含提示词的 Codex 模型兼容元数据，完整 system/developer prompt 不进入仓库资产或 CodeyRuntime 二进制。Codex 当前要求自定义模型目录的每个条目都保留 `base_instructions`；因此 Codey 只从用户本机已有的官方 `models_cache.json` 派生运行目录，原样保留本机缓存中的必需字段，并把生成文件权限收紧为仅当前用户可读写。缺少兼容的本机缓存时不生成不完整目录，官方线路回退 Codex 内置目录，第三方线路仍可完成上游模型探测与子代理能力校验；这是可恢复的内置目录回退，不记录为补丁失败。这类本机派生内容不得写入日志、测试夹具、发布包或版本库。

## 配置与路径

- Codey 配置：由 `directories` 根据系统保存到 Codey 配置目录下的 `config.json`。
- cc-switch 配置：自动发现 `~/.cc-switch/cc-switch.db`，仅同步 `app_type = codex` 的 provider。官方 ChatGPT 登录 provider 只读展示，Codey 不读取或改写其中的 OAuth token。
- Codex 配置：使用 Codex 默认 `CODEX_HOME`（通常是 `~/.codex`）。
- Trace 写盘防护不设开关：macOS / Windows 使用相同启动时机自动更新 Codex 根目录及旧版 `sqlite/` 目录中现有的 `logs_*.sqlite`，不会创建、清空或压缩日志库。
- Windows 卡顿补丁不设开关：Codey 在运行时识别 Windows，并在每次启动 Codex 时自动隔离 Micro 设备模块和周期性 WMI 进程采样。首次应用或版本升级后应先从系统托盘完全退出已有 Codex，确保补丁能在新主进程执行前安装。macOS 不执行 Windows 专属分支。
- 宠物硬阉割：`slimCodexPet` 默认为 `true`，macOS / Windows 都会在下次通过 Codey 启动 Codex 时生效。启用时若主 bundle 的语义锚点因官方升级而变化，补丁会失败关闭并停止 Codex，不会降级成仅隐藏 UI；关闭后下次启动会恢复完整宠物功能。
- 语音精简：`slimCodexVoice` 默认为 `false`，macOS / Windows 都会在下次通过 Codey 启动 Codex 时生效。开启后同时覆盖旧听写和新版 GPT Voice / Realtime Voice；关闭时保留完整语音功能。
- 浮动额度：`showAccountUsageInHeader` 默认为 `true`，保存后立即生效且不要求重启。只有活动线路被识别为官方账号登录时才请求并展示，切到第三方线路后保留开关值但停止请求和显示；用户手动关闭后的持久化值不会被默认值覆盖。
- Codex 慢启动保护：`fastCodexStartup` 默认为 `true`。Codey 会在 Electron 主进程仍处于启动暂停阶段时，为登录后的 Statsig bootstrap 设置 1.5 秒上限，并保留 renderer 保护作为兼容兜底；正常响应保持原流程，慢请求或失败请求会让 Codex 使用自身错误降级路径继续挂载主界面。原始初始化仍可在后续刷新中恢复；关闭后下次启动完全使用 Codex 原生等待策略。
- FastCtx 上下文工具：`fastContextTools` 默认为 `false`。打开后下次启动 Codex 生效；应用临时配置前会检查 `mcp_servers`，只要已有 server 的 ID、`command` 或 `args` 以独立 token 命中 `fastctx`（大小写不敏感），就完整保留并复用现有配置，不再注册 `codey_fastctx`，也不追加 Codey 专属 namespace、输出预算或工具指引。未检测到时，Codey 才在本次运行的临时 `config.toml` 中注册当前 Codey 主程序作为本地 STDIO MCP，并通过 `--codey-fastctx-mcp` 参数进入只运行 FastCtx 的服务模式；FastCtx 及其 o200k 分词器数据直接编入 Codey 主程序，不再分发独立 sidecar。升级时，带该参数标记的旧 Codey 自有 sidecar 配置会改写为当前主程序路径，同时保留并发写入的未知字段。内置 server 使用 FastCtx 自身的 8500 token 预算，并在用户没有配置 Codex 工具输出上限时设为 10000 token，随后追加工具使用指引；退出时随 provider 配置一起恢复原文件。关闭开关时还会幂等清理带 `--codey-fastctx-mcp` 标记的内置 server、作为完整段落出现的新旧两版 Codey FastCtx 固定提示词，以及能与这些 Codey 自有项共同确认的 `mcp__codey_fastctx` namespace；用户自己的 server、无法证明归属的 namespace、输出上限和其他提示词保持不变。FastCtx 通过 MCP tools 接口调用，工具名由 server ID 决定，例如 `fastctx` 对应 `mcp__fastctx__read`；文件路径传绝对路径，`resources/read` 属于另一套 MCP 接口。
- 子代理协作优化：`subagentOptimization` 默认为 `false`。关闭时配置页不允许手动切换子代理模型或推理档位；切换线路时会按新线路的已知模型支持范围重置为默认子代理模型与默认档位，默认模型不可用时回退到当前目录首个可用模型。开启前会校验当前线路是否支持当前子代理模型；第三方线路会实时刷新上游模型列表，不支持或无法确认时保持关闭并提示。打开后下次启动 Codex 生效；`config.toml`、`AGENTS.md` 与 `agents/default.toml` 的变更纳入同一个运行时租约，退出时自动恢复。`config.toml` 使用三方合并回滚 Codey 拥有的字段，提示词只移除 Codey 注入的完整块，用户运行期间替换过的 `default.toml` 不会被覆盖。
- Codex App 路径：可在 Codey 配置界面填写；留空时使用 CodeyRuntime 的平台发现逻辑。Windows 自动发现失败或已保存路径失效时，会在启动阶段打开原生目录选择器并持久化规范化后的应用目录，因此自定义盘符不依赖尚未启动的 Codex 页面；目录解析兼容安装根目录下的 `app`、`bin`、`current` 与 `versions/current` 布局。
- CDP 默认端口：`9229`，如 Windows 端口被占用会按 core 的逻辑选择可用回环端口。

### 通知渠道扩展

通知实现按“公共调度 + 渠道适配”拆分。后端 `backend/src/notifications/` 中的配置、事件、格式化和调度器不依赖具体渠道；每个发送渠道放在 `channels/` 的独立文件中，实现 `NotificationChannelAdapter`，并在 `channels/mod.rs` 注册。新增渠道时需要同时补齐渠道枚举与配置字段、请求构造、明确的成功响应校验、传输与响应错误脱敏及对应单元测试；HTTP 成功但响应损坏或缺少渠道成功字段仍按发送失败处理。

前端 `src/notifications/` 以 `channelRegistry.tsx` 为唯一渠道注册入口，每个渠道使用独立编辑器组件；注册项负责显示信息、默认配置和完整性判断，公共列表只负责展示、编辑和删除，启用状态与测试发送都在渠道编辑弹窗内配置。新增和编辑必须先完成渠道配置，并经不落盘的 `test_notification_channel` 测试成功后才能保存；每次修改草稿都会要求重新测试。外部配置结构继续使用 `webhook.channels`，既有 `test_webhook` 仍保留以兼容已有渲染层调用和持久化数据。涉及凭据的渠道必须保持普通配置返回渲染层前脱敏、留空保存时回填旧值、显式清除时不回填；仅在用户主动打开某一渠道编辑弹窗时，可经 `reveal_notification_channel` 按需返回该渠道凭据，弹窗关闭后立即清空本地草稿。

## 启动与恢复

打开 Codey 后不会创建常驻原生配置窗口；仅当 Windows 无法解析 Codex 应用路径时，启动阶段会显示一次系统目录选择器。Codey 会先检测 CC Switch 路由接管；非接管模式再迁移非法的内置 provider 覆盖，随后永久同步 rollout 与 SQLite、清理幽灵任务索引，备份并临时应用运行时配置、修复插件市场、启动 Codex，最后通过 CDP 注入轻量控制脚本。Windows 上必须先从系统托盘完全退出已有 Codex，自动性能补丁才能在新主进程执行前安装；macOS 上启用宠物硬阉割时也必须先完全退出已有 Codex。首次 Codex 启动失败时，Codey 会调用与正常退出相同的运行时停止和配置恢复逻辑，失败后等待 100 毫秒重试一次；Windows 随后通过阻塞任务显示原生错误对话框，用户关闭对话框后当前 Codey 进程返回错误并退出，不进入常驻关闭等待。首次点击 Codex header 中的 “Codey” 按钮时才会加载紧凑 React 浮层，配置操作通过本次 CDP bridge 发送给 Rust 进程。遮罩空白处、右上角关闭按钮和 `Esc` 都能关闭浮层。关闭这次由 Codey 拉起的 Codex 后，Codey 会先标记退出、取消并等待尚未执行完的延迟重启任务，再停止路由 overlay watcher，终止该 Codex 的主进程、Helper、app-server 及后代进程树，恢复临时配置，最后清理其他遗留 Codey 进程并自行退出；收到系统退出信号和安装更新时也执行同一套清理。遗留 Codey 清理只接受与当前程序完整路径一致的首次进程快照，并在每次终止前复核 PID 的启动身份；轮询期间不会吸收新进程，避免同名程序或 PID 复用导致误杀。会话 JSONL、数据库与索引清理结果不回滚。若 CDP 注入失败，Codey 会停止本次启动、显示原始错误并退出，不会另起本地 Web 服务。

Codey 不改写 `auth.json`，因此 Codex 的账号栏仍会显示原来的官方登录账号；这只代表客户端登录会话，不代表第三方 provider 仍走官方接口。读取本地活动线路时，provider 范围内的 `experimental_bearer_token` 优先于 `auth.json` 中的 API Key；读取 cc-switch 数据库时则优先采用该 provider 自己保存的 `auth.OPENAI_API_KEY`，并兼容 config 中的 token 回退。非路由模式运行期间全局 provider ID 保持不变，第三方 API 地址、协议和 bearer token 会直接写入该 provider 的临时配置；路由模式则完整保留 CC Switch Live provider 表与接管 token。

如果 Codey 异常退出，下次启动前会检查 `codex-lease.json`；非路由租约会在 provider 仍保持上次由 Codey 应用的 API 地址时先恢复备份，再应用当前线路。路由租约不以 provider ID 或地址作为恢复前提，而是从最新滚动基线中只撤销 Codey-owned 字段，避免 CC Switch 热切换 provider 后因保护性早退而遗留 overlay。新租约使用原始、已应用和当前配置做三方合并；缺少已应用快照的旧租约只回滚旧版本明确拥有的 provider、模型目录、推理档位与 FastCtx 字段，并保留插件、市场、用户新增键及同表中的并发扩展，不再整文件覆盖或删除当前配置。非路由模式若用户在 Codey 运行期间手动改写了 provider 或地址，恢复逻辑会保守地不覆盖该修改。Codey 自身的所有 `config.json` 读改写事务共用一把异步写锁；整份设置保存还携带持久化的 `settingsRevision`，旧页面或并发请求提交的过期快照会被拒绝，避免静默覆盖已经完成的配置更新。启动备份目录采用保留策略：应用运行时配置前清理 `codex-backups` 下最旧的启动备份（保留最近 5 份及当前租约引用的目录）；CC Switch 路由快照在租约滚动更新成功后立即删除被替代的一份。

## 已知限制

- 目标是 Codex Electron 桌面客户端，不覆盖 CLI。
- Windows 新版卡顿补丁针对 Codex Micro / Work Louder 设备集成导致的原生模块异常，以及当前客户端的周期性 WMI 遥测采样；Windows 上会自动启用，不会连接 Codex Micro 硬件，也不会启动该遥测 Worker 或 PowerShell。插件 app-server 在清理旧进程时可能执行的一次性 WMI 查询仍保留，避免产生孤儿进程；它不是 30 秒反复调用的来源。宠物硬阉割与 FastCtx 上下文工具保留用户开关。
- 当前 Codex 优先按 `threads.rollout_path` 定位 JSONL，并按 `task_started.turn_id` 删除整轮记录；旧版 `messages`、`thread_items`、`items` SQLite schema 作为兼容路径。
- 内嵌 FastCtx 当前只发布文件读取、搜索、发现与批量替换工具，不发布其可选 Bash/后台任务组；PDF 引擎未编入 Codey，PDF 应继续使用 Codex 自带的 PDF 能力。
- 第三方线路必须提供 Codex 原生支持的 Responses API；Codey 不再接受已移除的 `wire_api = "chat"`，也不提供 Responses/Chat Completions 协议转换。
- 页面注入使用稳定的 `data-*`/`electronBridge.sendMessageFromView` 探测，Codex bundle 大幅改版时可能需要更新选择器适配层。
- 消息通知按渠道列表保存，支持同时配置多个飞书 Webhook 与 Telegram Bot；旧版单飞书配置在读取时自动迁移。飞书接受官方或企业内网主机名的 HTTPS 机器人地址，仍要求 443 端口、标准 `/open-apis/bot/v2/hook/...` 路径且禁止 URL 用户信息、查询参数和片段；通知专用 HTTP 客户端不跟随重定向。`session.completed` 由真实 Codex turn 的完成状态触发，不再把单次模型 HTTP 响应误判为任务结束；失败、等待介入与手动测试仍保留。自动通知会并发投递到所有已启用且配置完整的渠道，并汇总失败；只有连接拒绝或渠道明确返回失败等确定结果才会自动重试，HTTP 超时、响应读取中断及其他没有明确失败响应的传输错误一律视为远端可能已经接收，停止重试并保留本次去重记录。等待介入通知采用写前持久化去重：先原子记录预留再请求渠道，确定失败时回滚；因为飞书与 Telegram Webhook 都没有可依赖的幂等键，进程在预留后、确认响应前崩溃时会保守地抑制重发，边界为 at-most-once。waiting 去重台账按插入序持久化并封顶 2048 条，超出时淘汰最旧键；台账写盘在阻塞线程执行且不占用状态锁。完成/失败通知使用当前进程内的有界去重历史，不承诺跨进程 exactly-once。飞书不保存或发送签名密钥；飞书 Webhook 地址与 Telegram Bot Token 默认不会返回渲染层，并通过配置状态保留已有凭据。用户主动打开单一渠道编辑弹窗时，后端才会临时回显该渠道凭据，弹窗关闭即清空本地草稿。所有通知消息都不包含 prompt、正文、内部会话 ID、线路 ID 或 API Key。
- 首版明文 API Key、飞书 Webhook 地址与 Telegram Bot Token 仅依赖配置文件权限保护，后续可把 `ConfigStore` 的 secret 存取替换为 macOS Keychain/Windows Credential Manager。

FastCtx 集成基于 [yc-duan/fastctx](https://github.com/yc-duan/fastctx) `0.2.4` 的固定提交 `86dac0c`（Apache-2.0）。
