# Codey 内部开发文档

本文档面向开发和维护人员，只保留当前架构、开发流程、关键边界和已知限制。用户可见功能维护在 README.md；历史方案和逐版本改动由 Git 记录，不在这里累积。

## 核心设计

- Codey 是 Rust 桌面辅助进程，负责启动、监控和停止官方 Codex Electron 客户端。
- 配置界面由 React 实现，构建后嵌入 Codey，并通过 CDP 注入 Codex 页面；通常没有独立常驻配置窗口。
- 本地路由开启时，Codex 只连接本次启动的回环网关。官方账号沿用 Codex 登录，第三方线路使用 Codey 保存的凭据。
- 线路、模型和上游格式分别识别。模型选择器使用带线路信息的稳定 ID，由本地路由在转发前还原为供应商原始模型 ID；关闭本地路由后，历史标识由会话恢复入口还原。
- Codey 配置与 Codex 配置分开保存。用户 Codex 配置原则上只读，只允许维护 Codey 自有的路由恢复桩和清理旧版 Codey 遗留项。
- 无法确认线路、模型归属或兼容能力时应停止请求并给出错误，不猜测、不跨线路自动切换，也不重放可能已经送达的请求。

## 目录

- src/：Codey 控制台、请求日志页和前端状态逻辑。
- public/：注入 Codex 页面的轻量脚本。
- backend/src/：启动器、配置、CDP、本地路由、会话、通知、诊断和更新实现。
- backend/resources/：随二进制分发的运行时规则数据。
- vendor/CodeyRuntime/：跨平台启动、配置和会话数据能力。
- scripts/：开发、构建、前端打包、更新清单和发布脚本。
- tests/ 与 backend 各模块测试：JavaScript 集成测试和 Rust 测试。
- .github/workflows/：质量检查与桌面安装包构建。

## 本地开发

需要稳定版 Rust、Node.js 22 和 pnpm。首次进入仓库先安装依赖：

    pnpm install

常用开发命令：

    pnpm run dev
    pnpm run check
    pnpm run test:js
    cargo test --workspace

pnpm run dev 会先构建完整 Cargo 工作区，再启动 Codey，确保主程序和 FastCtx sidecar 同目录。Windows 若检测到同一构建产物仍在运行，会要求先正常退出；只有确认进程卡死时才使用 CODEY_DEV_FORCE_KILL=1。

## 检查与构建

提交前至少执行：

    pnpm run check
    pnpm run test:js
    pnpm run vite:build
    cargo fmt --all -- --check
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    git diff --check

完整构建使用：

    pnpm run build

该命令先重建前端与注入脚本，再进行 Rust release 构建。macOS 会额外生成 target/release/bundle/macos/Codey.app；Windows 安装包由 .github/workflows/build-desktop.yml 使用 NSIS 生成。CI 的实际门禁以 .github/workflows/ci.yml 为准。

Windows x64 发布任务通过 CARGO_PROFILE_RELEASE_LTO=thin 和 CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 覆盖默认的 fat LTO 与单代码生成单元，以减少 release 优化和链接耗时；macOS 及本地构建沿用 Cargo.toml 默认配置。Windows 的 Rust 测试、Clippy 和格式检查继续保留。此调整可能影响二进制体积和运行性能，实际提速幅度需由下一次 Windows Actions 构建确认。v0.9.18 的参考耗时为 Windows 任务 12 分 43 秒，其中可执行文件构建 10 分 17 秒、NSIS 打包 36 秒。

macOS 本地调试未签名安装包时，确认来源后可用 `xattr -dr com.apple.quarantine /Applications/Codey.app` 移除隔离属性。此操作不会补齐签名或公证，发布包仍需单独处理。

## 发布

发布脚本会同步 package.json、Cargo.toml 和 Cargo.lock 的版本，运行检查，创建提交与标签并推送：

    pnpm run release -- 0.9.13

默认要求工作区干净。确实要把现有改动纳入发布时使用 --include-existing-changes；只在本地创建标签时使用 --no-push。

v* 标签会触发 macOS arm64、macOS x64 和 Windows x64 构建，并附加到 GitHub Release。配置以下 GitHub 变量和密钥后，工作流也会上传安装包与 latest.json 到 Cloudflare R2：

- CLOUDFLARE_R2_BUCKET
- CLOUDFLARE_R2_PUBLIC_BASE_URL
- CLOUDFLARE_ACCOUNT_ID
- CLOUDFLARE_API_TOKEN

CODEY_UPDATE_BASE_URL 可在编译时覆盖客户端更新源。发布标签版本必须与项目版本一致。

## 运行流程

1. 恢复上次异常退出留下的 Codey 自有临时状态，并执行启动更新检查。
2. 加载 Codey 配置，只读检查 Codex 配置、登录状态和应用位置；首次空配置可导入当前第三方线路。
3. 在 Codex 未运行时完成会话索引维护、旧版 Codey 状态清理和诊断保护准备。
4. 按设置启动本地路由、生成本次进程覆盖、Hook、子代理角色和注入脚本。
5. 启动 Codex，通过启动补丁或 CLI 包装入口传递本次 app-server 配置，再通过 CDP 安装桥接与页面增强。macOS 的 `CODEX_CLI_PATH` 指向私有可执行包装脚本，由脚本恢复可能被 Codex 子进程过滤的兼容环境后再进入 Codey CLI 包装分支，禁止把完整 Codey 桌面入口直接暴露为 CLI。包装器使用官方 CLI 的 `-c` 参数，执行目标程序后才完成握手；握手证明目标已执行，不代表 app-server 已完成初始化或接受了所有配置。Inspector 不可用时，以已确认的 CLI 包装入口正常运行；两条入口都失败且存在必须的运行时约束时停止 Codex。
6. 启动健康检查、退出监听、通知和平台保护任务。设置保存后，支持热更新的项目立即替换；影响启动参数、角色集合或能力目录的项目标记为需要重启。
7. Codex 退出、系统信号或安装更新时，先确认受控 Codex 已停止，再关闭 watcher、回收 Child、恢复临时配置，最后停止路由。停止进程失败时保留 watcher、桥接、配置和路由；配置恢复失败时保留路由，使同一运行时可以重试。只有清理完成后才释放 Hook、租约及其他 Codey 自有运行状态。

启动任一步失败都应走同一清理路径。会话数据的安全修复不会在退出时回滚；临时路由、Hook 和运行文件必须可恢复。

### 启动与补丁核验基线（2026-09-05）

Codey 当前声明版本为 0.9.18，不固定安装某一版 Codex。macOS 根据应用位置启动桌面客户端，CLI 包装器的目标来自该应用的 Resources，不能用 PATH 中的 `codex --version` 代替桌面运行版本。

本次本机证据：`/Applications/ChatGPT.app` 的 Info.plist 与 app.asar/package.json 均为 26.901.22334，构建号 7746，签名标识 com.openai.codex、TeamIdentifier 2DC432GLL2；运行主进程及 app-server 均来自此应用。内置 CLI 为 0.153.0，PATH 中独立安装的 CLI 为 0.145.0。包内开发依赖声明 Electron 42.3.0，实际 CDP 报告 Chromium 152.0.7977.64；不能把开发依赖版本当成定制运行时版本。

已读取早期标签 0.2.0（e8082e6）和 0.2.1（48937a3）：两版都传递 `--inspect-brk` 并把主进程补丁失败视为启动失败，WMI Worker 拦截已存在，尚无 Git 请求保护。两版之间主进程补丁文件未变，页面注入改为延迟加载会话工具；没有旧 Codex 安装包，无法证明当时实际二进制是否开放 Inspector。CLI 兼容入口由 e8d485b 于 2026-09-03 加入，2f844dd 随后隔离包装器环境，Windows Store 使用 86b1af4 的用户目录运行文件暂存方案。

本次实际进程携带 Inspector 参数，但对应端口拒绝连接；renderer CDP 可用。Codex Framework 的 Electron fuse wire 为 v1、9 项、`010011001`，`EnableNodeCliInspectArguments` 为关闭状态。由此确认本机主进程 Inspector 不可用的直接原因。保持只读检查，不改写 fuse、应用包或签名。桌面 bundle 的 `src-BXVxNf6C.js` 中仍有 `CODEX_CLI_PATH` 解析及 app-server 子进程入口，实际 app-server 参数包含 Codey 的运行时覆盖。

当前保留 renderer CDP 页面增强和 CLI 包装器运行配置；主进程 Inspector 可用时还会安装桌面统计上报和定时状态采集精简、窗口聚焦触发的插件刷新去重、任务标题模型处理，以及模型/页面控件兼容等可选修改。CLI 包装入口不安装这些主进程修改。

Inspector 与 CLI 包装器是内部启动路径，不是用户可切换的运行模式。任一入口安装成功即返回 `ready`；页面继续按实际功能探针显示正常、待确认或异常，不再把 CLI 路径显示为兼容模式或声称所有优化均已生效。只有 Windows 在两条入口均失败、且没有必须的运行时约束时，才可按基础参数重新启动并返回 `degraded`，界面显示需检查和具体原因。必要配置或子代理约束无法确认时仍中止启动。`performanceStatus`/`performanceDetail` 保留现有接口名称，当前表达启动健康状态。官方文档公开的 [codex app](https://developers.openai.com/codex/cli/reference) 用于打开客户端，[app-server](https://developers.openai.com/codex/app-server) 用于客户端协议；`CODEX_CLI_PATH` 按当前 bundle 的兼容入口维护，不标为官方稳定扩展 API。

### 性能补丁删除与依赖审查（2026-09-05）

按维护者明确要求，彻底删除 WMI 周期采样 Worker 拦截、主进程和 renderer Git 请求限流、临时 WebView 生命周期管理、Codex 执行环境及子代理的额外回收补丁、avatar overlay 预加载改写与隐藏窗口限速。同步删除状态 IPC、自检、renderer 探针、平台筛选、预览状态、专属脚本和已失效的测试。上述行为交回 Codex 自身处理，删除决定不代表已确认所有上游性能问题都已修复。

WMI 拦截自 0.2.0 存在；Git renderer 保护由 3280462 引入，主进程 IPC 由 1a0c4c7 引入。审查时上游 `worker.js` 已有 `sharedRuns` 去重、`repositoryRuns` 排队与 watcher 复用，缺少当前 Windows 实机证据。历史实现可从 Git 查询，当前代码不再保留兼容分支或等待命中状态。

Windows Store 运行文件暂存、CLI 环境隔离、Inspector 启动时防止 Worker 继承调试参数，以及用户可选的 Trace/Crashpad 管理仍有独立用途，予以保留。

依赖审查结合三个 Cargo 包、前端清单、构建脚本、平台 cfg 和源码调用；`cargo-machete .` 未发现未使用的直接依赖。`pnpm why @mantine/hooks` 确认它是 Mantine Core 的必需 peer，删除根声明不会减少安装树；`cargo tree --locked -i zopfli -e features` 确认 ZIP 的 deflate 特性同时由 FastCtx 启用，仅调整本项目不会移除 Zopfli。系统代理、系统证书、二维码、压缩包读取及原生平台依赖均保留，未改动依赖版本或锁文件。此次删除减少注入代码及随包资源，不宣称减少第三方依赖数量。

上轮审查清理复用 `http_response::read_bounded_body`，删除模型列表的重复限长读取实现；保留声明长度和分块读取的双重限制。发布脚本及标签打包流程均执行带锁定依赖的 Rust 测试和 Clippy，不能假设只监听 master/PR 的 CI 已验证标签。

此次删除后的 macOS 验证：`pnpm run check`、`pnpm run test:js`（348 项）、`cargo fmt --all -- --check`、`cargo test --workspace --locked`（1591 项）、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`pnpm run build`、`git diff --check` 全部通过。计数来自共享工作区，包含其他任务尚未提交的模型测试。回归确认原生 Worker 和 IPC handler 不再被 WMI/Git 补丁拦截，用户脚本隔离、CLI 启动和剩余页面增强保持通过。

release 应用通过 `plutil -lint`、`codesign --verify --deep --strict` 和可执行权限检查；使用临时 HOME/CODEX_HOME，经 release 包装器调用内置 CLI 0.153.0，app-server 的 initialize 请求成功且退出码为 0。未重启当前桌面会话。`cargo check --workspace --all-targets --locked --target x86_64-pc-windows-msvc` 在 ring 的 C 编译阶段因本机缺少 Windows SDK 的 `assert.h` 失败，不计为 Windows 验证通过；原生 Windows 测试由发布/CI 工作流执行，仍需实际运行。

## 配置与数据

- Codey 配置由 directories crate 放在系统配置目录的 config.json，并保留三份有效滚动备份。Unix 下配置、备份、日志和本地请求日志应限制为当前用户可读写。
- CODEX_HOME 非空时始终优先；否则使用 Codex 默认目录。
- auth.json 只读，Codey 不修改官方登录凭据。
- config.toml 在启动准备和正式启动前做快照复核。除 Codey 自有 codey_router 恢复桩和明确识别的旧版污染外，不改写用户 Provider、MCP、模型或未知字段。
- codex-lease.json、hooks.json 中的 Codey 组、角色运行副本和证明状态均属于临时运行资产，异常退出后由下次启动恢复。
- 第三方 API Key、通知地址和机器人令牌目前仍以明文保存在 Codey 私有配置及备份中；后端不会把已保存值返回前端。后续若迁移系统凭据库，应同时处理备份格式和升级兼容。
- codey-errors.log 只记录脱敏后的失败信息。不要把提示词、响应正文、认证值或完整敏感地址写入日志。

## 主要子系统

### 线路与模型

官方模型目录包含 `gpt-6-astra`，优先使用本机 Codex 缓存中的运行参数与推理强度；内置兼容元数据不包含提示词。GPT-6 的第三方线路别名仅在原模型模板声明 Ultra 和多代理能力时保留对应能力，通用第三方模型不继承这些参数。运行时只校验本次生成的模型条目；旧缓存缺少 GPT-6 模板时仍可使用已有模型，使用 GPT-6 前需直接启动官方 Codex 刷新缓存。

local_router.rs 维护不可变线路快照，按明确线路元数据、带线路的模型 ID 和可信会话绑定解析请求。保存线路后只影响新请求，已有流继续使用原快照。

模型选择器采用 `percent_encode(provider_id)/upstream_model`，用于区分不同线路上的同名模型；显示名称和短名称不参与标识。`modelAliasHistory` 在配置规范化、线路保存和删除前记录已发布别名与原始模型的对应关系，删除线路或关闭路由后继续保留，不保存凭据。旧配置缺少该字段时自动补齐当前已知别名；升级前已经删除且没有记录的任意前缀不做推断，仅对旧 `codey/` 格式保留兼容入口。

解析先匹配当前有效别名及原始模型，再按完整历史别名还原一次，使用现有线路提示、官方模型优先规则、会话绑定和唯一候选规则选择同一模型。比较沿用模型目录的大小写无关规则，转发保留该线路配置的原始拼写。真实模型名称可以含 `/`，历史还原结果只按原始模型查询，避免递归解释成另一条线路。无候选或存在无法消除的歧义时返回可操作的错误，不替换成无关默认模型。默认模型和子代理配置在原线路失效时也优先迁移到同一模型。

渲染目录通过 `legacy_model_aliases` 发布兼容记录；注入脚本同时兼容没有该字段的旧目录，并记录本次页面见过的别名。线程绑定仍使用原来的 v1 数据格式，恢复后按需写回有效线路；未能解析的绑定继续保留，避免省略模型的恢复请求错误采用全局默认值。关闭本地路由时，历史请求在恢复阶段还原模型并切换到当前原生 Provider；仅当前模型目录确认支持时放行，迁移失败不会标记成已成功。正常原生请求保持原有参数，不批量改写 rollout 或 SQLite 历史。

配置热更新替换路由快照并清理 WebSocket 负缓存；Prompt cache key 已包含线路和上游模型，迁移沿用隔离规则。兼容处理只发生在发送前，不提供请求失败后的跨供应商重试。回归测试位于 `model_id.rs`、`config.rs`、`local_router.rs`、`commands/models.rs` 和 `tests/codey-model-whitelist-inject.test.mjs`，覆盖带前缀与原始模型、删除/停用、重命名/切换、重启恢复、原生模式、歧义和旧数据。

本地路由负责官方与第三方认证隔离、模型 ID 还原、流式转发和上游格式适配。图片生成请求按明确线路元数据、会话绑定或全局默认模型的顺序选择线路，并只向 OpenAI 兼容上游透明转发 Images API，不尝试转换为 Anthropic Messages。WebSocket 能力由共享 Provider 开启，再通过模型目录的 `prefer_websockets` 按线路选择；支持的线路使用 WebSocket，不支持的线路继续使用 SSE。第三方线路的网页搜索、自动审核和远程压缩能力只有在配置与模型目录都能确认时才启用。本地路由关闭时，不安装路由覆盖，模型列表使用当前 Codex Provider 的可用模型，历史任务在恢复入口完成模型兼容转换。

仅在主进程 Inspector 可用且标题补丁安装成功时，Codey 才调整 Codex 自动标题模型：优先使用可用官方账号的 `gpt-5.6-luna`；没有官方账号时，依次使用当前默认第三方线路的同名模型和当前默认模型，推理强度保持 `low`，请求失败则保留客户端临时标题。CLI 包装入口沿用 Codex 自身的标题生成行为。

路由层不做跨供应商故障转移、轮询或自动重试。长连接只允许在请求尚未发送时回到同一线路的普通流式请求；发送后失败直接返回。

### 控制台与页面注入

cdp.rs 负责准备嵌入资源、安装桥接、首次注入和健康复核。src/overlay.tsx 挂载 React 控制台；public/ 中的脚本分别处理模型、插件、会话、提示词和平台增强。

控制台首次打开时再加载完整界面。轻量健康探针持续确认桥接状态，只有确定桥接缺失时才重注入；页面忙或探测超时保持保守状态。

用户脚本在同一文档成功执行后不会因桥接恢复而重复运行，失败可重试，新文档正常运行。内置脚本保留自身的恢复逻辑。桥接安装检查 Runtime.evaluate 的 exceptionDetails；失败释放新连接，成功替换后关闭旧 pump。new-document 注册随各次 CDP 会话维护，不使用跨连接 target 缓存。

模型列表热更新通过 QueryClient 发布新结果，不直接修改 React 共享查询对象，确保已挂载的对话模型选择器收到通知。WebSocket 和原生网页搜索线路的模型集合变化仍需重启生效。

Codex 更新后，优先检查启动补丁、app-server 参数结构、入口资源和页面语义选择器。兼容判断必须唯一命中，不能用宽泛文本或 DOM 位置猜测。

### 会话与插件

启动期会话维护只在受控 Codex 停止后修改 rollout、SQLite 和索引。运行中的导入、导出、删除轮次与恢复备份会先释放目标会话，再使用临时文件、大小限制、原子替换和并发校验；无法稳定确认轮次或当前页面已切离时拒绝删除。

插件市场修复使用随程序分发的快照和回滚替换。状态读取保持只读，只有用户触发修复时才更新 Codey 管理的市场目录与注册项。

### 提示词、子代理与 FastCtx

提示词优化可使用运行中的 Codey 路由，也可使用独立配置。地址、认证和模型由后端校验；日志不保存提示词正文或凭据。

子代理增强只在原生 macOS 和 Windows 启用。五个用户角色与内部 default 角色的配置源位于 backend/src/codex_config_guidance.rs，默认规则数据位于 backend/resources/subagent-rules.default.json。关闭本地路由时，启动器会按当前 Codex Provider 的可用模型重新校正角色模型与思考深度，再生成本次运行配置。当前路径直接使用 Codex 原生 agents 工具和生命周期 Hook，不再使用旧版 sidecar、逐任务回执、prepare_delegation 或 resolve_batch 流程。只读任务最多并行三个；出现写入角色时最多两个，并由根代理在所有尝试结束后验收结果。活动 attempt 全部通过绑定、marker 和 `files.read` 能力校验时，可信根 turn 可继续使用规则确认的本地读取、网页检索、MCP Resource 与数据库 schema/只读 SQL 工具；SQL 只接受单条、可保守证明为只读的语句，写入、命令、视觉、未知工具以及 writer/mixed/unverified 批次仍保持关闭。角色名、词法 SQL 校验和 Hook 不是最终安全边界，真实权限仍由数据库只读账号以及 Codex sandbox 与 approval 设置决定。

完整且无筛选的 agents 列表若只包含根代理，会精准回收从未绑定、从未启动的 pending spawn，覆盖 provider 在线程上限等失败后缺少 PostToolUse 回执的路径；已绑定或已启动 attempt 不受影响。顶层 wait 超时和仅根代理快照不算语义进展，不得重置 Stop 的 10 分钟停滞恢复窗口，只有带具体代理身份的状态或输出变化才会重置。

视觉角色由原生任务胶囊授予 `visual.inspect`，受信的图像、截图、CUA 和 `open_in_codex` 工具只对视觉角色开放。Responses 工具结果中的图像在 Chat Completions 与 Anthropic 回退协议中会转换为紧随 tool result 的用户图像块，不能退化成 base64 JSON 文本。协作响应中的解密失败、空 payload 或任务体缺失统一触发一次活动代理任务重述恢复；Codey 不尝试本地解密 provider 载荷。

内置 FastCtx 只提供文件读取、搜索、发现和批量替换。检测到用户已有 FastCtx 时不重复注册；内置版本通过本次进程覆盖加载，不写入用户 Codex 配置。版本与固定提交以 Cargo.toml 和 THIRD_PARTY_NOTICES.md 为准。

### 通知与诊断

通知支持飞书、企业微信、Telegram 和微信 ClawBot，最多保存 32 个渠道。完成、失败和等待介入事件由真实任务状态触发；不确定是否已送达时不盲目重发。

Trace 与 Crashpad 保护由 Codey 的存储维护和后台任务执行，按用户设置及平台支持启用，不依赖主进程 Inspector；只处理各自允许范围内的诊断数据，不触碰会话和账号数据。

请求日志默认关闭，可在路由运行期间热启停。请求主路径只做有界、非阻塞观测，写入由独立线程完成；过载时允许丢弃记录，不能反压模型请求。日志区分路由前置耗时、上游首包和下游首内容，记录线路、模型、状态、Token 与脱敏错误，但不记录提示词、响应正文或凭据。控制台日志页查询 SQLite；内部仍保留 NDJSON sink 供调试。该日志只用于诊断，不用于计费、审计或 exactly-once 账本。

## 维护约束

- README.md 只写用户能感知的功能与必要注意事项；实现、构建、发布、路径和限制写在本文档。
- 新功能先复用现有配置事务、桥接、URL 校验、错误脱敏和原子文件工具，不建立第二套流程。
- 对 Codex 持久数据的写入必须有所有权证据、快照复核、备份和原子替换；不确定时保持只读。
- 网络请求必须有输入与响应上限，凭据不能进入错误文本、URL、前端状态或请求日志。
- 本文档描述当前稳定结构，不记录调参历史、已删除方案或逐版本迁移过程。

## 已知限制

- 只支持 Codex Electron 桌面客户端；页面和 bundle 大改时可能需要更新补丁与注入适配。
- 第三方线路只能使用目标服务可表达的能力；无法无损转换的请求会在发送前拒绝。
- 本地路由没有跨线路自动容灾，长连接中断后也不会重放已发送请求。
- 子代理 Hook 用于本地协作约束，不等同于操作系统沙箱。
- FastCtx 不提供 PDF、MCP Resources 或 shell 工具，这些任务继续使用 Codex 自带能力。
- 请求日志是尽力而为的诊断数据，异常退出或队列过载可能丢失尾部记录。
- 当前发布的 macOS 与 Windows 安装包可能未签名，正式分发前应补齐平台签名与公证。
- 若系统拒绝终止 Codex，已建立的运行时会保留依赖供停止重试；若首次启动尚未建好运行时就发生注入失败且无法终止进程，Codey 退出仍会关闭路由，需要人工退出残留 Codex 后重启。当前测试不能替代 Windows 实机或完整桌面重启验证。
