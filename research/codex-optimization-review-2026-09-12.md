# Codey / Codex 启动、运行性能与兼容性调研

调研日期：2026-09-12。范围：公开源码、官方文档、技术文章、GitHub Issues / PRs / Discussions，以及当前 Codey 工作区。本文是分析与实施建议，本轮未安装第三方补丁、未修改运行代码、未重启应用。

## 结论与证据等级

当前最值得推进的顺序是：**验证主进程注入能力 → 建立启动分阶段基线 → 获取尚未进入当前公开 CLI 版本的上游修复 → 对有实测证据的热点做小范围优化**。

有四个关键结论：

1. **当前正在运行的 Codex 已确认通过 `--require` 执行主进程补丁；fuse 状态不能单独作为其他构建的能力证明。** 当前主进程 PID `35516` 的执行 marker 已被启动器确认，页面资源改写标记也为 `true`，详见 2.1。Electron 官方文档和 v42.0.0 上游源码对 packaged app 的通用限制仍然存在，但不能直接推定当前 Codex 不支持 require；当前构建为何不同尚待源码核实。[S01][S02][S03][L01]
2. **公开 CLI 0.153.4 已包含两项优化，另三项较新修复尚未进入该版本。** 已有：Git 根目录探测限时、MCP 可选服务启动等待配置。尚未进入：休眠 MCP 绑定复用、受保护 Shell 快照缓存、Unix 僵尸进程识别。结论来自 Git 提交关系和关键源码对照，不是根据发布时间猜测；本机二进制是否带有未公开回移补丁仍待确认。[P01][P02][P03][P04][P05][V01]
3. **最有借鉴价值的社区实现是标题体积诊断、补丁唯一锚点校验、局部可见性节流。** 全局返回空 Git 结果、冻结所有隐藏 WebView、强制 GC、修改签名和完整性 fuse，不适合作为 Codey 默认优化。[R01][R02][R03][R05][D01]
4. **不少建议已经在当前 Codey 中实现。** 重复读取源码、renderer 改写缓存、fuse 扫描缓存、宠物预热、WMI Worker 筛选、部分心跳和焦点刷新已有处理，不能再次作为新收益计算。剩余机会需要根据启动、主进程、renderer 和 app-server 四层分别定位。

本文统一使用以下证据等级：

| 标记 | 含义 | 可得出的结论 |
| --- | --- | --- |
| **事实** | 已读官方文档、具体源码、API 状态、提交关系，或进行了本地静态检查 | 只覆盖对应版本、文件和检查范围 |
| **用户/作者报告** | Issue、Discussion、项目 README 中的复现或数据 | 是调研线索，不等于本机复现或维护者确认 |
| **源码推断** | 根据调用链和数据流分析风险、收益、适配方式 | 需要测试或运行数据确认 |
| **待确认** | 缺少当前构建、平台或运行证据 | 不作为默认开启补丁的依据 |

## 1. 检索范围与关键词

### 1.1 检索范围

- **GitHub 开源实现**：筛选 6 个相关仓库，固定到具体 commit 阅读；覆盖 Python/CDP 启动器、Electron loader、ASAR patcher、功能解锁工具和只有说明的仓库。
- **openai/codex 上游**：重点核验 6 个 PR、17 个 Issue 及部分评论，另读 Discussion #29949。对 PR 读取合并状态、完整变更文件，并与公开 `rust-v0.153.4` 比较。
- **官方资料**：OpenAI 配置、app-server、故障排查、更新管理；Electron 性能、fuse、环境变量、ASAR、BrowserWindow；Node 模块编译缓存。
- **可信技术文章**：web.dev 的 `content-visibility`、Chrome for Developers 的 Page Lifecycle API。
- **当前项目**：启动器、主进程 IIFE、CDP、renderer 注入、会话索引维护和现有回归测试。

检索通过 GitHub API、仓库源码及网页原文完成。未执行第三方安装器。仓库活跃时间、星数、搜索摘要没有作为性能或安全结论的证据。这是按相关性筛选的调研，并非穷尽所有开源项目；GitHub 状态以调研当日为准。

### 1.2 关键词

| 方向 | 使用的主要关键词 |
| --- | --- |
| 社区方案 | `codex performance patcher`、`codex-perf`、`codex desktop launcher`、`codex-plus-plus`、`patch-codex-fast` |
| 启动与队列 | `codex startup slow`、`app-server queue`、`plugin/list timeout`、`bundled-plugin reconcile`、`MCP dormant bindings` |
| 进程与系统采样 | `WMI PowerShell Win32_Process`、`Win32_PerfFormattedData_PerfProc_Process`、`child-process-snapshot`、`zombie`、`detect_msys_tty`、`ps sampler` |
| Git / Shell | `Git root discovery`、`stable-metadata`、`protected shell snapshot`、`capture cleanup` |
| renderer | `blank startup`、`CalculateNativeWinOcclusion`、`content-visibility`、`backgroundThrottling`、`WebLifecycleState frozen` |
| 状态与存储 | `thread title payload`、`state_5.sqlite`、`session storage growth`、`heartbeat RRULE CPU` |
| 注入兼容性 | `NODE_OPTIONS packaged apps`、`EnableNodeOptionsEnvironmentVariable`、`EnableNodeCliInspectArguments`、`enableCompileCache`、`ASAR fs.openSync` |

## 2. 当前项目与运行版本基线

### 2.1 已核验版本

| 对象 | 本轮读取结果 | 限制 |
| --- | --- | --- |
| Codey | `master`，HEAD `4000151 Add main-process performance patches` | 工作区存在未提交改动，包含并行任务修改；本文代码定位以读取时为准 |
| 桌面应用 | `/Applications/ChatGPT.app`，bundle ID `com.openai.codex` | 不凭应用显示名称判断注入能力 |
| 桌面版本 | `26.903.71938`，build `8576` | 本机为 macOS；不是 Windows 验证环境 |
| 内置 CLI | `codex-cli 0.153.4`，读取应用内 `codex --version` | 版本文本不能证明与公开源码逐字相同 |
| 公开对应 tag | `rust-v0.153.4` → `3d2ee51ca2d5db578f328aa75e20aa22c0197c9a` | 已解引用 annotated tag，并核对提交关系 [V01] |
| 本机 app.asar | 306,621,494 bytes；SHA-256 `58fef82480b9064e209b5b2fd934992e8d71515aea8084482369cfeaff1b8ee0` | 只读检查；没有解包重写或重新签名 |

本机 ASAR 字符串检查发现 `Win32_ComputerSystem`、`windowVisibilitySequence`、`addChildProcessFields`、`CODEX_SPARKLE_ENABLED`；未发现 `child-process-snapshot` 和 `Win32_PerfFormattedData_PerfProc_Process`。这是**静态字符串事实**，不能证明所有 WMI 调用均已消失，也不能外推 Windows 安装包。

**当前运行实例的只读验证：** 主进程 PID `35516`，启动于本地时间 2026-09-12 11:52:33。其 `open` 启动器保留了 `NODE_OPTIONS=--require=.../startup-require/5f37d9a3b406400fb056d61cfd208af3.js` 和独立 marker 参数，主进程命令行没有 Inspector 参数。11:52:33.578，Codey 确认 `executed` marker 中 PID 为 `35516`；CLI wrapper 的 PID `35760` 到 11:52:35.498 才记录启动。[L01] 日志事件沿用 `cli_wrapper_marker_confirmed` 名称，因为两条路径共用 marker 读取函数，但此处 PID 明确属于主进程。脚本在自身执行结束时写入该 marker，读取后会清理文件。随后通过已有 renderer CDP 只读求值确认 `__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__ === true`、renderer bridge 存在；说明资源改写也已有运行证据。应用自身启动日志同时记录 `packaged=true`。[L02] 本次没有重启应用或重新注入补丁；尚未直接读取主进程内每个性能补丁的匹配计数。

### 2.2 实现层次与已有优化

| 层次 | 当前入口 / 关键文件 | 已有行为 | 不应误算为新增优化 |
| --- | --- | --- | --- |
| 启动选择 | [launcher/process.rs](/Users/kim/Desktop/codey-f/backend/src/launcher/process.rs:207) | require 文件准备成功则优先使用；否则尝试 Inspector；CLI wrapper 提供配置兼容 | 已有路径优先级和实际模式展示 |
| fuse 读取 | [electron_fuses.rs](/Users/kim/Desktop/codey-f/backend/src/electron_fuses.rs:275) | 按二进制签名缓存 fuse wire，首次扫描放到 blocking pool | 不需要再加一套 fuse 扫描缓存 |
| 主进程补丁 | [codex_startup_patch.js](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.js:2225) | `Module._load`、CJS 编译钩子、Electron `app` 协议响应改写、spawn / Worker 包装 | require 与 Inspector 共享这份 IIFE |
| 启动文件读取 | [CJS 编译钩子](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.js:2306) | 复用 Node loader 读出的 source，`finally` 恢复 `_compile` | 已避免 Codey 和 Node 重复读取同一源码 |
| renderer 改写 | [源码指纹缓存](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.js:187) | 已有内容指纹、受限输出缓存、失败指纹缓存 | 不能再用只有 URL 的缓存替代内容失效判断 |
| 精简与系统采样 | [采样开关](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.js:130)、[具体补丁](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.js:1812) | 宠物 prewarm no-op、隐藏 overlay 节流、Windows WMI Worker 筛选、macOS 特定子进程采样调用改写 | macOS 补丁有路径一致性问题待查，见 M1 |
| renderer CDP | [cdp.rs](/Users/kim/Desktop/codey-f/backend/src/cdp.rs:422)、[codey-inject.js](/Users/kim/Desktop/codey-f/public/codey-inject.js:3907) | 注入阶段、deadline、最大 500 ms 重试间隔、共享 MutationObserver | 已有重试与观察器合并，不建议另加持续全 DOM 扫描 |
| 数据维护 | [launcher.rs](/Users/kim/Desktop/codey-f/backend/src/launcher.rs:260)、[session_index_cleanup.rs](/Users/kim/Desktop/codey-f/backend/src/session_index_cleanup.rs:111) | 启动前恢复维护锁、重施删除记录、索引清理；索引已有无变化快速返回、备份和并发修改检查 | 这些保护不能为了缩短启动时间直接挪到启动后 |

主进程脚本只能改变它能拦截到的 JavaScript/Electron 行为。MCP 连接管理、Rust Shell 执行器、app-server daemon 等 Rust 模块不能由 `Module._load` 替换。CLI wrapper 的参数注入也不等于修改这些内部实现。[S06]

### 2.3 已有本地测量的适用范围

上一轮对当前应用抽取的 20 个 `.vite/build/*.js` 做过 loader 前后对照：读取次数 **36 → 20**，读取字节 **8,687,641 → 7,933,737**，少读 **753,904 bytes**，20 份改写输出 SHA-256 一致。该结果仅证明这组输入下消除了重复读取、保持改写结果一致，**不是完整冷启动 A/B，不代表启动时间按相同比例下降**。

相关 JS 回归记录为 387 项：386 通过、1 跳过、0 失败，见 [测试记录](/tmp/codey-injection-js-tests.log:2333)。这是上一轮已有验证，本轮报告未重跑应用启动或性能测试；当前并行编辑之后的完整工作区也不能仅靠这份记录保证。

## 3. 候选开源方案及对比

以下源码固定 commit，日期是所读 commit 的日期，不是仓库页面的最近活动时间。

| 项目 | 固定版本 / 许可证 | 核心机制 | 与 Codey 的差异 | 采用判断 |
| --- | --- | --- | --- | --- |
| `clairernovotny/codex-perf` | `94ea680`，2026-05-07，MIT [R01] | Python 启动器、renderer CDP、标题维护、局部 CSS 和预取 | 不提供 Codey 的主进程注入与路由能力；直接维护本地标题数据 | 借鉴诊断和独立副本测试；不照搬后台持续改标题 |
| `ifBars/codex-performance-patcher` | `08df53e`，2026-08-15，MIT [R02] | Bun/TS + `@electron/asar`，复制应用后改主入口，隐藏 WebView 冻结 | 验证范围是 Windows Store 26.810.6296；需维护应用副本 | 可参考生命周期测试；不采用 ASAR 修改路径和默认全局冻结 |
| `ugarchance/codex-plus-plus` | `a80bb64`，2026-09-09，MIT [R03] | 多补丁流水线，ASAR 解包、入口注入、Acorn 校验、重包 | 功能扩展占主导，涉及包完整性与平台安装器 | 借鉴唯一锚点、语法检查；不移植安装方式 |
| `duanluan/codex-plus-plus-launcher` | `240a293`，2026-09-10；package.json 声明 MIT，未见独立 LICENSE [R04][R04c] | Python/npm 外壳，获取上游 Tauri sidecar 和注入资产 | 仓库没有完整 renderer 注入资产源码 | 仅作为分发/配置参考，不能据此验证主进程机制或性能 |
| `yangchuansheng/patch-codex-fast` | `56fd836`，2026-05-16，MIT [R05] | Python 改写应用，解锁 Fast/Plugins/Chrome 等功能 | 修改 ASAR、fuse、签名；Fast 指服务能力，不是本地启动性能 | 排除作为本项目默认方案 |
| `zhanglove2003/codex-desktop-performance-notes` | `0348ca4`，2026-07-17，未见许可证 [R06] | 只有简短 README | 没有可分析的补丁、源码或测试 | 排除为可移植实现，不据名称推断能力 |

另一个长期替代方向是直接使用 **官方开源 Codex app-server** 构建客户端。[S06] 它减少对私有 Electron bundle 的依赖，但意味着承担会话 UI、插件、授权、通知、远程连接等能力，不是低成本的性能补丁。Codey 当前任务不需要转为完整客户端重写。

## 4. 候选源码功能分析

### 4.1 codex-perf：标题异常与 renderer 加载

**事实。** `scripts/codex-perf-launch.py` 打开 remote-debugging-port，通过自带 WebSocket/CDP 实现执行 renderer 脚本，并注册新文档脚本；这条 `Runtime.evaluate` 属于 renderer CDP，不是 Codey 的 main-process Inspector evaluate。[R01a]

`renderer/fast-thread-loader.js` 的主要功能包括：[R01b]

- 对消息容器应用 `content-visibility:auto`、containment 和 intrinsic size，减少离屏布局/绘制。
- 点击任务时调用私有 `threads.read` 做有限预取，不读取完整输出。
- 每 30 秒扫描标题，对过长或默认标题做修复，并设置每任务冷却期。

`scripts/fix-codex-perf.py` 使用 Python 标准库 `sqlite3` 和文件 API，检查列结构、完整性、备份及标题异常，另有隔离 HOME 和数据库副本探针。[R01c]

**关键差异与风险。** `apply_repair` 先追加 `session_index.jsonl`，再提交 SQLite 更新；SQLite 事务不会回滚前面的 JSONL 追加。因此不能将这个实现描述为跨文件原子更新。它还按首条用户消息重建标题，可能改变用户自定义命名。Codey 已有锁、原始快照检查与备份，应复用这些保护，而不是直接引入另一套修复脚本。[R01d]

**作者报告，未在本机复现。** README 报告标题 payload 15.6 MB → 25 KB、SQLite median 8.12 → 0.15 ms、JSON 编码 46.34 → 0.14 ms。这说明极端标题异常值得诊断，不能证明普通用户也有相同收益。[R01e]

**适配结论。** 先实现只读统计：标题总字节、最大值、分位数、标题与首条消息相同的异常数量。修复只在确认异常且能够保留自定义标题时提供。不要增加默认每 30 秒遍历全部任务的维护器。

### 4.2 ifBars：隐藏 WebView 节流和冻结

**事实。** `src/asar-patch.ts` 将 Store 应用复制到可写目录，改 `package.json.main` 为 loader 并保留原入口。`src/loader-source.ts` 监听 WebContents / BrowserWindow，为隐藏的 `codex-sandbox` WebView 在等待后发送 `Page.setWebLifecycleState(frozen)`，显示时恢复 active；配置还包含 renderer 数量限制和冻结后的 GC。[R02a][R02b]

optimized 配置使用 12 的 renderer/数量阈值、2 秒检查、30 秒冻结等待。这些是项目默认参数，**不是当前 Codex 的实测最优值**。[R02c] `diagnose.ts` 自身运行 PowerShell WMI 采集数据，不能把它当成消除 WMI 调用的实现。[R02d]

**官方边界。** `backgroundThrottling` 会影响 timer、动画和 Page Visibility；同窗口任一 WebContents 禁用节流，会影响整个窗口绘制。冻结更进一步：timer、fetch callback 等任务会暂停，应用应处理连接、锁与状态保存。[S04][S10]

**适配结论。** Codey 已对明确识别的隐藏宠物 overlay 节流。只有在证明某类隐藏 WebView 没有任务、授权、网络回调、音视频或 Computer Use 活动后，才可试验冻结。仅凭不可见不能证明它处于空闲。

### 4.3 codex-plus-plus：补丁校验机制

**事实。** `patch/apply.mjs` 负责解包、唯一文件匹配、改写、Acorn 语法解析、重包；依赖包括 `@electron/asar`、`acorn`、`resedit`。[R03a] bootstrap 设置 `CODEX_SPARKLE_ENABLED=false`，macOS 安装器还处理 ASAR 完整性、Sparkle、`MallocNanoZone` 等。[R03b][R03c]

**可借鉴点。** 每个补丁限定目标文件与唯一锚点，改写后验证语法，不匹配就显式报告。Codey 已有局部匹配计数、optional patch failure 和内容指纹；应补齐版本样本与验证覆盖，优先用现有 JS 测试或 `node --check`，不必仅为单次语法检查新增生产依赖。

**不能借鉴的结论。** 设置 Sparkle 环境变量只证明启动器尝试影响更新路径，不能证明 Codey 已完整接管应用更新，也不能证明升级提醒、Store、内置 CLI 更新都受同一变量控制。[S11]

### 4.4 其余方案的限制

- **duanluan launcher**：`runtime.py` 复制 `renderer-inject.js` 等上游资产，`upstream_patch.py` 调整默认功能开关；仓库缺少被复制资产的完整实现，因此本报告不判断其具体注入时机。[R04a][R04b]
- **patch-codex-fast**：`app.py` / `patterns.py` 涉及 ASAR 备份改写、fuse 和 macOS 重签；与不更改签名/完整性 fuse 的要求冲突。功能解锁也不能计作 CPU、磁盘或启动性能优化。[R05a][R05b]
- **performance-notes**：只有说明，没有可审计实现，缺少有效移植依据。[R06]

## 5. Codex Issues、PRs 与 Discussion 调研

### 5.1 已有修复及当前版本关系

公开 CLI tag 为 `3d2ee51…`。下表通过 GitHub compare 核验祖先关系，并对比关键实现。公开版本包含某个 PR，不保证本机相关功能路径已启用；公开版本未包含某个 PR，也不能排除桌面发行二进制的私有回移。关键对照源码见该版本的 [MCP tool catalog][V04]、[Git discovery][V05]、[PID backend][V06]。

| PR | 截止调研时状态 | 核心实现与源码位置 | 与公开 CLI 0.153.4 的关系 | Codey 处理方式 |
| --- | --- | --- | --- | --- |
| [#44121][P01] | 2026-09-09 已合并 | `codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs`、`runtime.rs`；区分 Ready/Dormant catalog revision，复用绑定，服务启动或目录变化后失效 | **尚未包含此修复**：提交分叉，tag 中仍是旧 `stable_catalog_revision` 实现 [C01] | 升级兼容 CLI 后做 MCP catalog 构建次数和工具正确性 A/B；不在 JS 层仿造 Rust 缓存 |
| [#43954][P02] | 2026-09-09 已合并 | `core/src/environment_selection.rs`、`session/turn_context.rs`、`shell_snapshot*.rs`、`exec.rs`、network-proxy；受保护快照按环境/shell/cwd/sandbox 区分，最多 32 项，改进持有管道的后代清理 | **尚未包含此修复**：提交分叉，tag 没有新增 sandbox 文件，所读快照实现不同 [C02] | 优先升级获取；不得只缓存 shell 文本而省略凭据、环境与沙箱失效条件 |
| [#42132][P03] | 2026-09-01 已合并 | `utils/git-discovery` 合并同 cwd 的进行中探测，最多 8 个并发；`core/src/turn_metadata.rs` 对相关元数据等待限定 1 秒；不永久缓存非 Git 结果 | **已包含**：merge commit 是 tag 祖先，源码也有并发上限和 1 秒等待 [C03] | 不重复回移；Electron 自己的 Git worker 是否仍重复请求要单独分析 |
| [#43504][P04] | 2026-09-07 已合并 | `codex-rs/app-server-daemon/src/backend/pid.rs`；检查 `ps stat` 的 Z 状态，校验 PID 起始时间，适当 `waitpid(WNOHANG)` | **尚未包含此修复**：提交分叉，tag 对应读取只请求 `lstart=`，没有新增 Z 分支 [C04] | 用升级修复 daemon 状态识别；不能宣称解决全部 MCP、Electron zombie 问题 |
| [#41199][P05] | 2026-08-27 已合并 | 可配置 `mcp_optional_startup_grace_ms`；连接管理、配置 schema 和专项测试同步修改 | **已包含**：祖先关系及 schema 均确认 [C05][V02] | 可以做可选 A/B；**0 会回到逐服务 timeout 等待，不是完全不等待** |
| [#31471][P06] | open，未合并 | ConnectorRuntimeManager；按账号、用户、workspace 模式、Codex home 隔离工具快照；schema v4、32 MiB 读取上限、mtime 与原子替换 | 不能作为已发布修复 | 观察后续整组 PR；这是 1/4 重构，单独移植成本高、收益未定 |

**收益边界。** PR #44121 与 plugin/list 排队存在技术相关性，但没有证据证明它直接解决 #44401；#42132 作用在 Rust 元数据探测，也不能直接宣称解决 Electron 非 Git 工作区循环。合并、进入发行版、运行路径命中、用户症状改善是四个不同事实。

### 5.2 Issues 与已有 workaround

| 议题 | 状态 / 报告环境 | 已知事实与证据强度 | 对 Codey 的启发 |
| --- | --- | --- | --- |
| [#36025][I01] | closed；旧 Windows WMI/鼠标卡顿 | 维护者评论称 26.730.7989.0 修复部分主要问题；后续用户仍报告其他卡顿 [I01c] | 保留精确 WMI 兼容逻辑；不要把所有卡顿归因于 WMI |
| [#22912][I02]、[#25453][I03] | open；旧版 WMI 类问题 | 用户报告，不能外推当前 26.903 | Windows 运行时统计命中数后再决定是否扩展匹配 |
| [#44401][I04] | open；Windows 26.903.8094.0 | 报告 5–6 in-flight、64 个 interactive 排队、plugin/list 约 30 秒超时；原因仍含假设 | 区分队列等待、MCP 生命周期与网络耗时；不要通过增加 renderer 缓存掩盖 app-server 阻塞 |
| [#34244][I05] | open；Windows 26.715 | bundled-plugin reconcile 时 plugin/list 约 61 秒阻塞，用户报告 | Codey 已有 focus reconcile 补丁，仍需核验新版锚点与实际调用次数 |
| [#19568][I06] | closed，**duplicate** | 非 Git 工作目录触发 renderer CPU / metadata 循环的报告 | 可研究窄范围 Git metadata 去重；关闭原因不代表修复已发布 |
| [#37236][I07]、[#37240][I08] | closed，**duplicate**；macOS | zombie / 文件描述符增长报告 | 与 daemon Z 状态修复分开验收；不能全局杀进程或重写 ps 输出 |
| [#38754][I09]、[#30408][I10] | open；stdio MCP 反复启动/未释放 | #30408 作者报告超过 9 GB RSS；本机未复现 | 记录每会话 server spawn/exit、取消后的残留，不默认禁用 MCP |
| [#41783][I11] | open；Windows Git shim | 用户定位孙进程在 `detect_msys_tty/NtQueryObject` 卡住，直接 child 清理遗漏 | 只清理由当前实例拥有的进程树；目录探测缓存不能替代进程回收 |
| [#43263][I12] | open；Windows 26.901.6511.0 | 报告主 UI DOM 缺失，reload 有效，GPU/occlusion flags 无效 | 建立 DOM/连接就绪诊断；不能只凭白屏统一修改 GPU 参数 |
| [#42547][I13] | open；Windows 26.831/26.901 | 报告 renderer 已绘制但窗口未显示，occlusion flag 或 reload 有效 | 与上一项表现不同；GPU/遮挡 workaround 应版本和症状限定 |
| [#42648][I14] | open；存储增长跟踪 | 多种 session、trace 等机制叠加增长 | 做分类体积诊断和明确保留策略；不把清空历史或关所有日志当默认优化 |
| [#30248][I15] | open；macOS 26.623.31921 | 作者隔离到一条 ACTIVE heartbeat RRULE，停用后恢复；不代表每条过期规则都会复现 | 可做有限只读诊断和单条恢复；保留其他自动化 |
| [#26989][I16] | open；26.602.40724 | 用户报告旧全局 UI 状态导致 renderer 循环；4,175 字符草稿仅是可疑因素，未证明单一因果 | 不以草稿长度直接删除状态，也不能与 RRULE 问题混为一谈 |
| [#32516][I17] | open；Windows 26.707 | 用户报告无 Git/WMI 仍持续高 CPU | 强化分层测量，避免继续扩大 Worker 拦截范围 |

### 5.3 Discussion #29949 的价值与代价

[Discussion #29949][D01] 对旧 Windows 26.707.3351.0 做过组合补丁实验：作者报告 WMI 26 → 0 次/分钟、Git 206 → 22 次、鼠标最大延迟 234 → 16 ms。[D01c]

同时修改了 WMI snapshot Worker、git-origins 返回、共享 process snapshot、registry upsert/complete 等多个行为。**这不是单个补丁的受控对照**，无法分配各项收益，也不能应用到当前版本计算加速比例。

源码思路的副作用明确：空 Git 结果可能丢失项目来源/远端信息，空 process snapshot 可能影响子进程列表与终止操作，registry no-op 可能丢失状态持久化。空结果还可能使调用方重试。可借鉴其测量方法，不建议恢复整组 no-op。[D01]

## 6. 可实施补丁、优先级与集成成本

成本是相对估算：小为复用现有入口的局部修改；中为跨启动/诊断/界面的改动与平台测试；大为替换 CLI、移植上游 Rust 或依赖私有生命周期。收益均未承诺固定百分比。

### 6.1 高优先级

#### H1. 修正注入能力判断与执行证据

- **位置**：[启动选择](/Users/kim/Desktop/codey-f/backend/src/launcher/process.rs:207)、[macOS 选择](/Users/kim/Desktop/codey-f/backend/src/launcher/process.rs:504)、[require 等待](/Users/kim/Desktop/codey-f/backend/src/launcher/process.rs:1479)、[执行 marker](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.js:2501)、`MaintenanceStatus` / `OperationsPanel`。
- **事实 / 推断**：当前运行实例的 require 已执行，但启动选择仍只依据 fuse 和 payload 准备结果；上游 packaged app 的过滤行为说明跨构建不能只靠这一条件判断。若另一构建的 require 未执行、CLI 成功，当前流程直接返回 CLI，**运行失败后并没有自动获得 Inspector 主进程补丁**。[S01][S03][L01]
- **实现思路**：保持成功确认过的 require 为优先路径；按应用二进制版本/签名记录成功与失败，升级失效。明确区分预期方式、已执行方式、具体补丁匹配结果。对未确认的 require 路径，允许在受控启动阶段彻底停止该尝试后再试 Inspector；不在已开始用户任务的进程上偷偷重启，也不同时装两套 hook。
- **证据完善**：现有 marker 在 IIFE 末尾写入，证明脚本执行和 hook 安装，不证明之后加载的所有 bundle 均匹配。沿用现有 main-bundle/per-patch 状态，补充到诊断；核验 marker 的进程归属、版本和本次启动身份。CLI 先成功而 marker 未到的情况应保持准确降级状态，若增加后续确认，不能拖慢首屏或假定迟到 marker 必然出现。
- **预期收益**：避免无效 require 尝试占用启动预算，恢复能够使用 Inspector 的构建的主进程补丁；消除显示已注入但实际只有 CLI 的误解。
- **验证**：原生打包 Electron、当前 Codex、fuse on/off、环境未应用、marker 缺失/迟到、CLI 成功、Inspector 不可用、应用退出；同时证明无双重 hook。**成本中，优先于继续堆加 JS 补丁。**

#### H2. 补齐启动和运行阶段的低开销测量

- **位置**：[launcher.rs](/Users/kim/Desktop/codey-f/backend/src/launcher.rs:260)、[spawn_and_inject_runtime](/Users/kim/Desktop/codey-f/backend/src/launcher.rs:1588)、[cdp.rs](/Users/kim/Desktop/codey-f/backend/src/cdp.rs:422)、现有 diagnostic log、主进程 patch counters。
- **现状**：已经有 CDP 阶段、deadline、fuse scanMs；缺少统一的成功路径耗时和阶段关联，不能把现有错误阶段报告当完整启动 trace。
- **实现思路**：复用现有诊断日志，用单调时钟记录配置校验、会话维护、fuse/文件准备、spawn、主进程确认、CDP bridge、首屏可交互；只记耗时、数量、错误类型。app-server 队列与网络请求耗时单独计量，不能用 Codey HTTP 路由耗时替代 native app-server 排队时间。
- **预期收益**：决定下一项优化应在哪层实施，发现重复重试和非必要串行工作。该项本身不是承诺加速。
- **验证**：成功、降级、超时、取消路径均能还原时间线；诊断不开全量日志/全盘扫描，不输出会话正文；测量开启与关闭的额外 CPU/I/O 可接受。**成本小至中。**依据：[S05][I04][I17]。

#### H3. 为三项尚未进入公开版本的修复建立兼容升级验证

- **位置**：现有 CLI 解析、wrapper 路径及 [launcher/platform.rs](/Users/kim/Desktop/codey-f/backend/src/launcher/platform.rs)、`codex_startup_patch.rs`；若确需源码回移，修改的是独立上游 Rust 构建，不是 Codey IIFE。
- **实现思路**：选择已含 #44121 / #43954 / #43504 的官方后续构建，核对 Desktop/app-server 协议、命令行与配置 schema。优先整体应用升级；只有 Codey 已支持的明确 CLI 替换入口且协议验证通过时，才使用独立 CLI。保留原始可执行文件和配置恢复途径。
- **预期收益**：减少 MCP 重复绑定、同环境 Shell 快照重建、失效 PID 误判；仅在对应路径活跃时产生收益。
- **验证**：MCP 冷/热目录、工具变更、账号切换、required 服务；Shell cwd/env/凭据/沙箱切换、后代持管道取消；daemon 活进程、Z 进程、PID 复用。**升级接入成本中，源码回移成本大。**依据：[P01][P02][P04][C01][C02][C04]。

#### H4. 标题与索引体积只读诊断；异常用户优先

- **位置**：[session_metadata.rs](/Users/kim/Desktop/codey-f/backend/src/session_metadata.rs)、[session_index_cleanup.rs](/Users/kim/Desktop/codey-f/backend/src/session_index_cleanup.rs:111)、现有维护命令和设置页。
- **实现思路**：显式运行、schema 检测、只读连接、有限结果，统计字节与异常分布，不读取或上传正文。先复用既有维护入口。确认异常后再设计备份、并发检查及跨 SQLite/JSONL 的可恢复修复；不自动按固定长度重写用户标题。
- **预期收益**：识别标题 payload 导致的列表加载/序列化热点。正常标题分布下预期收益很小，因此修复是条件化高优先级。
- **验证**：大量短标题、极长标题、emoji、用户手动命名、未知 schema、活跃写入、修复中断与恢复；比较列表响应 bytes、序列化耗时和首屏。**诊断成本小至中，可靠修复成本中。**依据：[R01c][R01d][R01e][I14]。

### 6.2 中优先级

| 项目 | 修改位置与最小实现思路 | 预期收益 / 启动条件 | 成本、风险与验证 |
| --- | --- | --- | --- |
| **M1 统一精简策略与更新策略** | [prepare_startup_require_in](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.rs:165)、[IIFE 环境读取](/Users/kim/Desktop/codey-f/backend/src/codex_startup_patch.js:130)。macOS sampler 开关目前由 require 环境注入，Inspector 路径不天然等价；应由统一 patch options 决定。核验 Sparkle 消费者及子进程环境范围 | 保证注入方式切换后实际性能策略一致。更新禁用应独立于注入方式，不能当成免费性能收益 | 成本小至中。已看到清理 NODE_OPTIONS/marker，未看到这两个性能/更新变量的同等清理；需逐消费者判断，不直接删除。更新不能因优化失去正常获取安全修复的途径。[S11][R03b] |
| **M2 仅对重复 Git metadata 请求做进行中复用** | IIFE main/worker 编译改写处；先采样调用方法、cwd、持续时间，再确认具体模块。仅合并同键、同参数的只读请求，不修改 mutation | 在重复 `stable-metadata` / git-origins 是热点时减少进程和排队；Rust Git discovery 已优化，不能重复计算 | 成本中至大。优先只有 in-flight 复用；负结果缓存若需要必须短时、受限，并考虑 `git init`、worktree、HEAD/index/config 变更。无可靠锚点就不发布。验证 Git 信息、分支变化和失败重试。[P03][I06][D01] |
| **M3 可选的非零 MCP 启动等待调节** | `config.rs` 与现有 runtime overrides 构建处，使用官方 `mcp_optional_startup_grace_ms`，不新增 MCP 代理 | 对可选慢服务，试验 1000 ms 基线与较小非零值，例如 250/500 ms；可能更快生成初始工具目录 | 成本小，行为风险中。首轮可能暂时缺工具；required 服务必须保持。**0 不是快速模式。**使用当前版本 schema 验证，不全局改默认。[P05][V02][S07] |
| **M4 离屏消息布局节流实验** | `public/codey-inject.js` 的样式/消息容器入口；仅对已确认不受原生虚拟列表管理的长消息块应用 `content-visibility:auto` | 降低长任务切换和滚动时的 layout/paint；不减少服务端计算或 DOM 数据量 | 成本中。不能同时铺全页新 observer。验证虚拟列表、滚动锚点、流式追加、搜索、文本选择、打印和读屏；强制布局读取会抵消收益。[R01b][S09] |
| **M5 自动化和全局状态异常诊断** | 现有维护入口及 `launcher.rs`，以单项只读检查提供定位；只有复现后才处理指定任务或字段 | 针对启动即 CPU 100% 的特定状态问题，避免全量重置 Codex home | 成本中。不能因为 ACTIVE 且 UNTIL 已到就认定必然有死循环，不能删除长草稿。若将来修改同步 RRULE 求值，Promise timeout 无法打断同线程循环，需要真实迭代边界或隔离执行。[I15][I16] |
| **M6 启动前维护的剩余扫描成本** | `run_startup_session_maintenance`、`session_index_cleanup`、消息删除重放。先用 H2 分出耗时，复用现有 marker、候选 ID 和快照机制 | 仅在大型会话目录仍导致显著冷启动耗时时，减少与候选无关的读取 | 成本中。不跳过持久删除重放，不将所有维护挪到应用已加载状态之后，不在更新中用陈旧缓存删除数据。验收恢复、并发写入、未知 schema 与冷/热扫描一致性。依据为当前源码及 [I14] |

### 6.3 低优先级 / 暂不默认采用

| 项目 | 位置与实现思路 | 收益前提 | 暂缓原因与验收 |
| --- | --- | --- | --- |
| **L1 Node 原生 compile cache** | IIFE 入口通过能力检测调用 `module.enableCompileCache`；使用原生 API，不自建字节码缓存 | JS 编译确实占启动主要成本，实际 Electron 内置 Node 暴露 API，改写后的 CJS 路径也参与缓存 | Node 文档提示第一次可能变慢、跨版本不复用、覆盖率可能失真；随机 require 文件名也可能降低该文件缓存复用。不能把最新 Node API 签名直接用于旧 Electron。验证关闭/冷/热缓存、内容变化失效、升级、只读目录和磁盘上限。[S08] |
| **L2 隐藏 WebView 冻结** | IIFE 的特定 WebContents 生命周期，明确 URL/角色白名单，先采用普通 backgroundThrottling | 有真实空闲且长期隐藏的目标，显著消耗 CPU | 成本中至大。冻结会暂停 callback；需证明插件、授权、Computer Use、音视频、网络和保存状态正常。当前只保留窄 overlay 节流。[R02b][S04][S10] |
| **L3 任务预取 / 官方分页 API** | 当前 renderer 任务读取入口；若已有 Codey 专用 app-server 连接，优先官方 `thread/read`、分页 API 和按需订阅 | 快速切换耗时来自本地读取，且 app-server 队列空闲 | 不为预取新增独立常驻客户端。私有 `threads.read` 与公开 `thread/read` 不是同一契约；最新文档的实验字段须核对当前 schema。不在 Desktop 共用连接上盲目 unsubscribe/过滤通知。[R01b][S06][I04] |
| **L4 renderer 硬上限、强制 GC、广域空结果、更多全局日志关闭** | 来源于社区 loader/Discussion 的进程级策略 | 尚未证明当前负载受益 | 默认不采用：可能增加进程争用、GC 停顿、丢失 Git/子进程信息或排障证据。应先有单项数据及功能回归结果，不设想一个通用快模式。[R02c][D01][I17] |

### 6.4 macOS `/bin/ps` 与 Git Worker 的专项判断

**可以做限定调用点的优化，不宜做全局拦截。** 当前 macOS 补丁改写的是特定 `addChildProcessFields` 调用，属于减少诊断采样；它不等于所有 `/bin/ps` 都可以省略。上游 #43504 恰好依赖 `ps stat/lstart` 判别 zombie 和 PID 起始时间，说明全局返回空进程信息会妨碍正确的生命周期管理。[P04]

对 macOS 现有采样补丁，仍需验证任务进程列表、终止后台任务、退出回收、故障报告是否受影响；并解决 require/Inspector 的开关一致性。对 Git，保留原始结果语义的进行中请求复用优先于 Worker 返回空数组。暂时没有当前 Windows worker 的稳定源码锚点和请求量 A/B，不能给出可默认发布的整段 Worker 替换方案。[D01][I06][I11]

## 7. 风险、依赖与兼容性说明

| 边界 | 已有证据 | 集成要求 |
| --- | --- | --- |
| 打包应用启动能力 | 上游 Electron packaged allowlist 不含 require；当前 Codex 的主进程 marker 已确认执行，具体行为差异原因待查 [S01][S03][L01] | 当前实例按实测结论判断，其他构建重新验证；不翻转 fuse、不重新签名 |
| 主进程与 Rust 边界 | app-server 是独立进程/协议实现 [S06] | Rust 修复通过对应版本获取；CLI 配置成功与主进程补丁成功分别报告 |
| ASAR 与同步 I/O | `fs.open/openSync` 可能临时解包；ASAR `stat` 除大小/类型外不可靠 [S12] | 现有 WMI 大小门槛是合理优化；不为了限量读取改用导致解包的 API。包内内容变化不能只靠虚拟 mtime/ctime 判断 |
| 私有 bundle 锚点 | 社区和 Codey 都依赖版本相关源文本 [R03a] | 唯一匹配、语法验证、原文回退、版本样本；未知版本不能默认为已应用 |
| 用户状态 | SQLite 事务不涵盖 JSONL；当前维护有备份/快照检查 [R01d] | 只读诊断先行；任何修复可恢复、保留自定义标题/草稿/自动化 |
| 更新机制 | Codey 当前设置 Sparkle env；官方更新管理有独立政策与版本前提 [S11] | 不宣称禁用 Sparkle 已接管更新；区分 Desktop、Store、CLI 更新，保留更新渠道 |
| 后台可见性 | hidden 与 frozen 不等价 [S04][S10] | 先用原生节流，仅冻结已证明可挂起的对象 |
| 缓存隔离 | 上游快照/Connector 修复包含环境、账号和 revision 失效 [P01][P02][P06] | 不省略身份、路径、沙箱和配置失效，避免把过时工具/凭据带到新上下文 |
| 许可证与供应链 | 多数候选为 MIT；OpenAI Codex 为 Apache-2.0 [V03]；notes 未见许可证 | 移植代码保留原始许可和归属；固定 commit、检查发布资产来源，不执行未经审阅安装脚本 |

## 8. 测试与验收方案

### 8.1 分组与测量规则

至少区分三组：原版应用、当前 Codey、当前 Codey 加单个候选补丁。使用相同应用/CLI、硬件、电源模式、数据副本、插件列表、网络和模型；随机交错执行，分别统计首次缓存、热缓存与重启。建议每组至少 10 次报告中位数及范围；要解释 p95，应增加样本，例如 30 次以上，并说明仍有采样不确定性。

记录：启动到主进程、注入确认、CDP bridge、可交互首屏、首次本地操作；主/renderer/app-server CPU、RSS、进程数；源码读取 bytes；Git/WMI 调用次数；MCP 初始化与队列等待；日志/缓存写盘量。远端模型响应单独统计，不能算成本地启动补丁收益。

性能接受门槛应在实现前确定。先跑重复基线估计波动，只接受明显超过噪声、能重复、没有功能退化的改善；不依据一次截图或单个极端样本宣称加速。

### 8.2 验收矩阵

| 维度 | 必测场景 | 验收标准 |
| --- | --- | --- |
| 注入能力 | require 确认/过滤、Inspector on/off、CLI-only、环境未应用、marker 缺失/迟到、升级 | UI 与诊断准确；无双重 hook；无无限重启；每个补丁实际命中与否可查 |
| 启动维护 | 空目录、10k 级任务样本、索引不变/变化、未知 SQLite schema、并发写入 | 正确恢复已删内容的约束；不误删、不覆盖并发更新；热启动不增加全量扫描 |
| WMI/macOS sampler | 已知和改名 Worker、非采样 Worker、Sentry ComputerSystem、当前 Windows 与 macOS | 仅目标命中；不阻断非目标调用；进程列表、后台任务终止与退出正常 |
| Git | 普通仓库、非 Git 目录、运行中 git init、worktree/submodule、分支切换、慢磁盘、卡住的 shim | 结果与未补丁一致；并发重复减少；失败不无限重试，不误杀用户进程 |
| MCP/CLI 升级 | 无插件、多 stdio/HTTP、慢/失败/required MCP、取消、账号/工作区变化 | 工具目录更新正确，授权保持，子进程回收，不因较短 grace 永久缺工具 |
| Shell 快照 | cwd/env/login/凭据/sandbox 变化，子进程持有 stdout/stderr，合法后台进程 | 缓存正确失效；取消能退出；不泄漏旧凭据、不终止应保留的后台任务 |
| renderer/CSS | 长任务流式输出、滚动恢复、搜索、复制、选中、打印、读屏、浮窗显示/隐藏 | 无内容丢失或滚动跳动；布局/绘制耗时改善超过波动 |
| 白屏恢复 | DOM 未挂载与窗口遮挡分别复现，正在生成/等待授权 | 只在明确可恢复情形提供恢复；不无限 reload，不丢草稿或活动任务 |
| 更新与配置 | 应用升级、CLI 升级、require/Inspector 切换、关闭 Codey | 旧缓存失效；应用正常更新；临时环境和配置按所有权恢复 |

代码层测试优先复用 `tests/startup-lifecycle-patch.test.mjs`、`startup-patch-platform.test.mjs`、`windows-wmi-patch.test.mjs`、`codex-runtime-optimization-patch.test.mjs` 及 Rust launcher 测试。修改 Rust 启动相关文件后先 `cargo fmt --all`，再运行 `pnpm test:js` 和对应 Rust 测试。主进程改写还应针对当前提取样本进行语法检查、匹配计数和关键输出比较。

**发布条件**：测试通过不代替真实打包应用验证；高风险补丁必须有对应平台的单项 A/B 和明确关闭/回退方式。每项必须分别标明未命中、成功、失败，不能用一个总的 ready 状态代表所有优化。

## 9. 尚需补充的信息

以下信息缺失时，不应直接断言某个新补丁已能生效或会加速：

1. **更细的主进程状态**：当前 require 执行 marker、进程归属、页面资源改写已验证，应用日志也记录 packaged=true；仍需直接确认 Electron/Node 版本、各 main bundle 补丁的匹配计数，以及当前运行时与上游 NODE_OPTIONS 过滤行为不同的原因。无需导出完整环境变量或会话内容。
2. **Windows 对照环境**：Store/非 Store、具体应用与 CLI 版本、架构、实际 worker 源码特征与命令调用计数。本机 macOS ASAR 不足以证明 Windows 已无 WMI sampler。
3. **发行二进制来源**：内置 0.153.4 是否有私有回移、替换 CLI 是否受 Desktop 支持；公开 tag 对照只能约束公开版本。
4. **性能基线**：完整冷/热启动耗时、各进程 CPU/RSS、app-server 队列、插件数量与启动方式、Git 目录结构；目前只有局部 loader 测量。
5. **数据规模统计**：标题长度/字节分布、索引和数据库大小、任务数量、自动化数量；优先匿名聚合，不采集用户正文。
6. **功能依赖**：是否使用宠物、音视频、Computer Use、隐藏 WebView、后台终端、Remote Control；决定可否进一步节流。
7. **更新策略**：Codey 是否确有完整 Desktop 更新接管、Sparkle 开关实际消费者、企业 managed policy 支持；未核实前不能默认关闭所有更新行为。

推荐实施顺序：H1、H2 并行准备验证；完成后推进 H3；按数据决定 H4 与 M2–M6。M1 可作为兼容性整理提前处理。低优先级项保持实验性质。

## 10. 参考链接

### 官方文档、源码与技术文章

- [L01] 当前启动器执行确认及随后 CLI wrapper 启动记录；[L02] 当前应用自身启动日志。
- [S01] Electron 环境变量：packaged app 的 NODE_OPTIONS 限制。
- [S02] Electron fuses：Node options 与 Inspector fuse 独立。
- [S03] Electron v42.0.0 `node_bindings.cc`：`SetNodeOptions` 的 packaged allowlist；用于验证上游行为，不代表本机 Codex 的定制源码。
- [S04] Electron BrowserWindow options：backgroundThrottling。
- [S05] Electron performance：减少启动期同步 I/O、避免主线程阻塞。
- [S06] OpenAI app-server：连接初始化、线程 API、订阅与通知。
- [S07] OpenAI 配置参考：MCP grace、startup timeout、analytics 等。
- [S08] Node module：原生 compile cache 的 API、版本与限制。
- [S09] web.dev：content-visibility 的作用、布局与可访问性限制。
- [S10] Chrome for Developers：Page Lifecycle / freeze 的语义。
- [S11] OpenAI 桌面更新管理：支持范围、策略与更新责任。
- [S12] Electron ASAR：额外解包和虚拟 stat 限制。
- [S13] OpenAI Codex 故障排查：版本读取、日志和恢复入口。

### 开源方案固定源码

- [R01] codex-perf `94ea68005a13da87b44e97500a724cfdcf949c38`。
- [R01a] Python CDP 启动器；[R01b] renderer 实现；[R01c] 维护脚本；[R01d] 跨文件修复顺序；[R01e] 作者性能数据。
- [R02] ifBars patcher `08df53e51255eadbd61f62fa8811509cab09e8f5`。
- [R02a] ASAR 入口改写；[R02b] loader/冻结；[R02c] 默认参数；[R02d] WMI 诊断。
- [R03] codex-plus-plus `a80bb648f0cb9e27baaec09848d95c2d2005fb23`。
- [R03a] 补丁流水线；[R03b] bootstrap；[R03c] macOS 安装器。
- [R04] launcher `240a293b0a4a461b07e11302c188f2cbd8d719c9`；[R04a] 外部资产复制；[R04b] 默认功能调整；[R04c] npm 许可声明。
- [R05] patch-codex-fast `56fd83604aff1a39a92dc927d7f11e8b3e97bb95`；[R05a] 应用改写流程；[R05b] fuse 模式。
- [R06] performance-notes `0348ca47197b4850d0a6465bd8c64e82a489ea64`。

### 上游修复与版本验证

- [P01] #44121 MCP 休眠绑定复用；[P02] #43954 Shell 快照缓存/清理。
- [P03] #42132 Git 探测限时；[P04] #43504 Unix zombie 状态；[P05] #41199 MCP grace；[P06] #31471 ConnectorRuntimeManager，未合并。
- [V01] CLI 0.153.4 固定源码；[V02] 该版本配置 schema；[V03] Codex 许可证；[V04] MCP tool catalog；[V05] Git discovery；[V06] PID backend。
- [C01] #44121 与 tag 比较；[C02] #43954 与 tag 比较；[C03] #42132 与 tag 比较；[C04] #43504 与 tag 比较；[C05] #41199 与 tag 比较。

### Issues 与 Discussion

- WMI / CPU：[I01] #36025、[I01c] 维护者评论、[I02] #22912、[I03] #25453、[I17] #32516。
- MCP / 队列：[I04] #44401、[I05] #34244、[I09] #38754、[I10] #30408。
- Git / 进程：[I06] #19568、[I07] #37236、[I08] #37240、[I11] #41783。
- 白屏：[I12] #43263、[I13] #42547。
- 状态 / 存储：[I14] #42648、[I15] #30248、[I16] #26989。
- 社区组合实验：[D01] Discussion #29949、[D01c] 测量评论。

[S01]: https://www.electronjs.org/docs/latest/api/environment-variables#node_options
[L01]: /Users/kim/.codex-session-delete/codey.log:11647
[L02]: /Users/kim/Library/Logs/com.openai.codex/2026/09/12/codex-desktop-cfefa8e6-b534-4642-a97e-620f4361099b-35516-t0-i1-035233-0.log:1
[S02]: https://www.electronjs.org/docs/latest/tutorial/fuses
[S03]: https://github.com/electron/electron/blob/v42.0.0/shell/common/node_bindings.cc#L466
[S04]: https://www.electronjs.org/docs/latest/api/structures/browser-window-options
[S05]: https://www.electronjs.org/docs/latest/tutorial/performance
[S06]: https://developers.openai.com/codex/app-server
[S07]: https://developers.openai.com/codex/config-reference
[S08]: https://nodejs.org/api/module.html#module-compile-cache
[S09]: https://web.dev/articles/content-visibility
[S10]: https://developer.chrome.com/docs/web-platform/page-lifecycle-api
[S11]: https://developers.openai.com/codex/enterprise/manage-app-updates
[S12]: https://www.electronjs.org/docs/latest/tutorial/asar-archives
[S13]: https://developers.openai.com/codex/app/troubleshooting
[R01]: https://github.com/clairernovotny/codex-perf/tree/94ea68005a13da87b44e97500a724cfdcf949c38
[R01a]: https://github.com/clairernovotny/codex-perf/blob/94ea68005a13da87b44e97500a724cfdcf949c38/scripts/codex-perf-launch.py#L238
[R01b]: https://github.com/clairernovotny/codex-perf/blob/94ea68005a13da87b44e97500a724cfdcf949c38/renderer/fast-thread-loader.js#L59
[R01c]: https://github.com/clairernovotny/codex-perf/blob/94ea68005a13da87b44e97500a724cfdcf949c38/scripts/fix-codex-perf.py#L192
[R01d]: https://github.com/clairernovotny/codex-perf/blob/94ea68005a13da87b44e97500a724cfdcf949c38/scripts/fix-codex-perf.py#L920
[R01e]: https://github.com/clairernovotny/codex-perf/blob/94ea68005a13da87b44e97500a724cfdcf949c38/README.md#L85
[R02]: https://github.com/ifBars/codex-performance-patcher/tree/08df53e51255eadbd61f62fa8811509cab09e8f5
[R02a]: https://github.com/ifBars/codex-performance-patcher/blob/08df53e51255eadbd61f62fa8811509cab09e8f5/src/asar-patch.ts#L28
[R02b]: https://github.com/ifBars/codex-performance-patcher/blob/08df53e51255eadbd61f62fa8811509cab09e8f5/src/loader-source.ts#L218
[R02c]: https://github.com/ifBars/codex-performance-patcher/blob/08df53e51255eadbd61f62fa8811509cab09e8f5/src/config.ts#L3
[R02d]: https://github.com/ifBars/codex-performance-patcher/blob/08df53e51255eadbd61f62fa8811509cab09e8f5/src/diagnose.ts#L4
[R03]: https://github.com/ugarchance/codex-plus-plus/tree/a80bb648f0cb9e27baaec09848d95c2d2005fb23
[R03a]: https://github.com/ugarchance/codex-plus-plus/blob/a80bb648f0cb9e27baaec09848d95c2d2005fb23/patch/apply.mjs#L250
[R03b]: https://github.com/ugarchance/codex-plus-plus/blob/a80bb648f0cb9e27baaec09848d95c2d2005fb23/patch/patches/020-hub-bootstrap.mjs#L1
[R03c]: https://github.com/ugarchance/codex-plus-plus/blob/a80bb648f0cb9e27baaec09848d95c2d2005fb23/install/mac/install.sh#L73
[R04]: https://github.com/duanluan/codex-plus-plus-launcher/tree/240a293b0a4a461b07e11302c188f2cbd8d719c9
[R04a]: https://github.com/duanluan/codex-plus-plus-launcher/blob/240a293b0a4a461b07e11302c188f2cbd8d719c9/codex_plus_plus_launcher/runtime.py#L894
[R04b]: https://github.com/duanluan/codex-plus-plus-launcher/blob/240a293b0a4a461b07e11302c188f2cbd8d719c9/codex_plus_plus_launcher/upstream_patch.py#L7
[R04c]: https://github.com/duanluan/codex-plus-plus-launcher/blob/240a293b0a4a461b07e11302c188f2cbd8d719c9/package.json#L52
[R05]: https://github.com/yangchuansheng/patch-codex-fast/tree/56fd83604aff1a39a92dc927d7f11e8b3e97bb95
[R05a]: https://github.com/yangchuansheng/patch-codex-fast/blob/56fd83604aff1a39a92dc927d7f11e8b3e97bb95/scripts/codex_fast_patch/app.py#L98
[R05b]: https://github.com/yangchuansheng/patch-codex-fast/blob/56fd83604aff1a39a92dc927d7f11e8b3e97bb95/scripts/codex_fast_patch/patterns.py#L3
[R06]: https://github.com/zhanglove2003/codex-desktop-performance-notes/tree/0348ca47197b4850d0a6465bd8c64e82a489ea64
[P01]: https://github.com/openai/codex/pull/44121
[P02]: https://github.com/openai/codex/pull/43954
[P03]: https://github.com/openai/codex/pull/42132
[P04]: https://github.com/openai/codex/pull/43504
[P05]: https://github.com/openai/codex/pull/41199
[P06]: https://github.com/openai/codex/pull/31471
[V01]: https://github.com/openai/codex/tree/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a
[V02]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/config.schema.json#L6515
[V03]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/LICENSE
[V04]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs#L58
[V05]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/utils/git-discovery/src/lib.rs#L24
[V06]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/app-server-daemon/src/backend/pid.rs#L701
[C01]: https://github.com/openai/codex/compare/20f109eadb9b45360e6ca4f1dee2e82c83a48f7a...3d2ee51ca2d5db578f328aa75e20aa22c0197c9a
[C02]: https://github.com/openai/codex/compare/808b3411fdd0dd05d7a9f1c221a5bc87934943ac...3d2ee51ca2d5db578f328aa75e20aa22c0197c9a
[C03]: https://github.com/openai/codex/compare/8436b749a410133c58ec61fe89db07496ca22033...3d2ee51ca2d5db578f328aa75e20aa22c0197c9a
[C04]: https://github.com/openai/codex/compare/6750f5bd1356fe1553c0fcc9f2632704f3055946...3d2ee51ca2d5db578f328aa75e20aa22c0197c9a
[C05]: https://github.com/openai/codex/compare/124e560b9357746b6a7e2bba1fd244115fd9cc5a...3d2ee51ca2d5db578f328aa75e20aa22c0197c9a
[I01]: https://github.com/openai/codex/issues/36025
[I01c]: https://github.com/openai/codex/issues/36025#issuecomment-5186391855
[I02]: https://github.com/openai/codex/issues/22912
[I03]: https://github.com/openai/codex/issues/25453
[I04]: https://github.com/openai/codex/issues/44401
[I05]: https://github.com/openai/codex/issues/34244
[I06]: https://github.com/openai/codex/issues/19568
[I07]: https://github.com/openai/codex/issues/37236
[I08]: https://github.com/openai/codex/issues/37240
[I09]: https://github.com/openai/codex/issues/38754
[I10]: https://github.com/openai/codex/issues/30408
[I11]: https://github.com/openai/codex/issues/41783
[I12]: https://github.com/openai/codex/issues/43263
[I13]: https://github.com/openai/codex/issues/42547
[I14]: https://github.com/openai/codex/issues/42648
[I15]: https://github.com/openai/codex/issues/30248
[I16]: https://github.com/openai/codex/issues/26989
[I17]: https://github.com/openai/codex/issues/32516
[D01]: https://github.com/openai/codex/discussions/29949
[D01c]: https://github.com/openai/codex/discussions/29949#discussioncomment-17608321
