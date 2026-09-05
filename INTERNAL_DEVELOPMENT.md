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

Windows 的启动兼容安装最多尝试 2 次，仅超时、中断、WouldBlock 或明确的 Windows 文件共享/锁冲突（错误码 32、33）允许重试。目标程序无效、配置解析错误、权限拒绝、Inspector 响应不兼容和清理失败均不重试。首次 CLI 运行文件准备结束后建立一个共享截止时间，预算沿用已有的 20 秒 CLI 就绪等待加 24 秒配置验证，共 44 秒；Inspector 使用同一个 20 秒就绪窗口，避免其原有 15 秒等待提前结束后，Codex 仍停在 inspect-brk 状态；后续兼容等待和重试不重新计时。每次先准备包装入口，再选择 Inspector 端口。重试前必须成功清理本次进程和 Windows Store 临时环境，并再次确认预算尚未用尽。必要的进程清理保留独立的 20 秒上限；文件暂存和系统激活调用不通过取消 Future 强行中断，避免产生无法追踪的进程。因此 44 秒约束的是兼容等待与重试预算，不是整个应用启动的硬性耗时保证。

CLI 包装器在目标校验和创建进程前建立认证连接。令牌后的 EOF 仍只表示目标已执行；失败时发送 `!` 和最多 8 KiB 的结构化错误，保留具体原因与是否允许重试。收到明确失败立即结束兼容等待；创建进程不再使用独立的 750ms 确认窗口，改为共享启动截止时间。握手监听器只服务首次启动，其关闭后仍允许后续 app-server 调用 CLI。回归使用真实子进程覆盖目标缺失、配置无效、执行失败、参数和环境隔离、监听器关闭后的重启；退出码与监听器关闭后的重启回归复用测试程序作为固定返回 17 的原生子进程，避免让 CLI 配置参数参与 shell 命令解析；断言失败时保留子进程输出。Windows 测试另用独占文件句柄验证共享冲突分类。重试分类、截止时间和立即返回通过 Rust 行为测试覆盖，源码检查只保留平台清理顺序等约束。

诊断日志记录 Store 临时环境启用与清理、激活返回的 PID、线程恢复结果、Inspector 发现、CLI 认证和执行确认；环境只记录是否存在，不记录令牌或完整配置。仅凭 Inspector 与 CLI 同时超时无法确定一次现场故障的根因，需结合这些阶段记录及报错机器的完整错误日志判断。

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

启动维护不再把 rollout 和 SQLite 中的任务 Provider 批量改成 config.toml 的默认 Provider。该旧行为会使带第二条线路模型别名的任务直连第一条供应商，本地路由因未收到请求而没有日志。已有 `codey_router` 任务通过启动前写入的本地路由表恢复，其他任务沿用恢复入口的运行时迁移。保留陈旧锁、已删除消息和会话索引清理，删除不再使用的 Provider 同步缓存。恢复响应顶层 `modelProvider` 明确返回实际供应商时，以它为准；旧响应仅含 rollout 中的 Provider 时仍兼容成功请求的迁移记录。页面注入较晚、尚未取得任务供应商状态时，`turn/start` 同样要求先恢复任务，不能假定它已使用本地路由。回归检查覆盖配置、普通及归档 rollout、SQLite 保持原值，以及供应商未知或迁移响应明确返回旧供应商时阻止第三方模型请求。

本地路由负责官方与第三方认证隔离、模型 ID 还原、流式转发和上游格式适配。图片生成请求按明确线路元数据、会话绑定或全局默认模型的顺序选择线路，并只向 OpenAI 兼容上游透明转发 Images API，不尝试转换为 Anthropic Messages。WebSocket 能力由共享 Provider 开启，再通过模型目录的 `prefer_websockets` 按线路选择；支持的线路使用 WebSocket，不支持的线路继续使用 SSE。第三方线路的网页搜索、自动审核和远程压缩能力只有在配置与模型目录都能确认时才启用。本地路由关闭时，不安装路由覆盖，模型列表使用当前 Codex Provider 的可用模型，历史任务在恢复入口完成模型兼容转换。

仅在主进程 Inspector 可用且标题补丁安装成功时，Codey 才调整 Codex 自动标题模型：优先使用可用官方账号的 `gpt-5.6-luna`；没有官方账号时，依次使用当前默认第三方线路的同名模型和当前默认模型，推理强度保持 `low`，请求失败则保留客户端临时标题。CLI 包装入口沿用 Codex 自身的标题生成行为。

路由层不做跨供应商故障转移、轮询或自动重试。长连接只允许在请求尚未发送时回到同一线路的普通流式请求；发送后失败直接返回。

### 本地路由延迟与协议审查（2026-09-05）

首轮只调整共享 SSE 分帧状态和请求日志计时，新增可重复的本地基准。不改变选路、认证、请求字段、工具映射、流式事件、错误码、超时、重试、连接上限或日志结构。维护性改动和性能证据保存在本节及 `backend/benches/local_router_results.json`；取消传播与终态处理的后续进展见下节。

调用链与延迟边界：

1. Codex renderer 通过 app-server 发起请求，`codex_startup_patch.js` 在发送入口同步补充线路信息；运行时 Provider 使用本次启动的回环 Responses 地址。已有 owner 恢复等待和完成后 reconciliation 不属于普通新请求的 token 转发路径。
2. `LocalRouter::start` 创建共享 HTTP 客户端、路由快照、会话绑定和并发/请求体配额；入口检测 HTTP 或 WebSocket，先鉴权再读取请求体。HTTP 一连接处理一个请求，WebSocket 会话可承载后续轮次。
3. Responses 请求按需要解压、解析 JSON，按显式模型、路由元数据、会话绑定和默认配置选路，再还原模型别名并设置上游认证。大 JSON 的解析和转换已有 blocking worker；原生 Responses 未变更时保留原始请求字节。
4. 原生 Responses 直接转发；Chat Completions / Anthropic Messages 分别转换多轮消息、工具、图片、上下文和输出结构。非流式客户端等待上游完整结果属于 JSON 契约，不能通过提前返回局部内容缩短该等待。
5. 上游 HTTP 已复用连接池、开启 TCP_NODELAY 和 HTTP/2 自适应窗口；WebSocket 在线路和身份一致时按会话复用，有心跳和失败退避。仅在请求尚未发送时允许同线路降级，发送后不重放。未增加跨线路重试、并发模型调用或响应缓存。
6. 上游数据经过类型识别后立即逐块转发，适配流在完整 SSE 帧到达后生成 Responses 事件。已有嗅探会在前缀足以判定时继续，不固定等待 1 KiB；每个 HTTP chunk 合并为一次写入。Codex 原生 renderer 消费最终事件，Codey 注入层不拦截 token 流。

影响与处理顺序如下。收益评估针对具体触发条件，不能直接等同于所有对话都会获得的加速。

| 顺序 | 问题及影响范围 | 影响 / 收益 / 风险 | 本次处理 |
| --- | --- | --- | --- |
| 1 | 首字前取消未并发中止上游等待，连接名额可能继续被占用；等待上游响应头或空闲流时尤其明显 | 高 / 高 / 高 | 仅方案：将连接关闭信号接入发送和读取 future，先验证 TCP 半关闭、WS 会话和取消日志语义；不修改所有权和请求生命周期 |
| 2 | 一个未完成的 SSE 帧跨多个网络分片时从头重复扫描，事件越大，CPU 和邻近请求调度延迟越明显 | 高 / 高 / 低 | 已实施：共享分帧器记录已消费位置和已扫描位置，只扫描新增内容并重看最多 3 字节边界 |
| 3 | 请求日志把后台观察任务的排队/解析时间算入首内容和完整耗时，妨碍性能判断 | 中 / 高 / 低 | 已实施：传递分片成功写出后的时间戳；首次结束时固定总耗时，仍等待观察任务补齐 usage、状态和错误后只提交一次 |
| 4 | Chat SSE 缺少 `[DONE]`、Anthropic SSE 缺少 `message_stop` 时，部分 EOF 路径仍会合成成功结束 | 高 / 稳定性收益高 / 中高 | 仅方案：明确对不发送结束标识的兼容上游如何处理，再补终态检查；直接收紧会将既有可接受响应变为错误 |
| 5 | Anthropic 非 2xx 固定映射为 502；适配 WS 在已发 `response.failed` 后返回 Err，外层可能再发失败事件 | 高 / 协议收益高 / 中高 | 仅方案：确定客户端错误契约后复用安全错误摘要，并显式区分已提交终态的错误；不能用吞掉 Err 来消除第二个事件 |
| 6 | Chat / Anthropic 流式路径同时保存输出和累积器状态，结束时重建整份响应提取 usage / incomplete 信息 | 中 / 长回复收益中 / 中高 | 保留。重建过程还涉及工具名还原、JSON 参数验证和错误时序，不能直接删除；后续以长文本、并行工具、无效参数的逐事件等价测试先确定边界 |
| 7 | 每个转换后 SSE 事件经过 JSON 字符串、SSE 字符串和 HTTP chunk 缓冲；部分无线路元数据的头也会重新编码 | 低 / 微小 / 低中 | 保留。先做分配计数再决定是否合并编码；本轮不改请求头原有规范化结果 |
| 8 | 下游 HTTP 使用 `connection: close`；错误日志每次独立 `spawn_blocking`；429 等状态的 reason phrase 可能回落到 OK | 低至中 / 场景相关 / 中高 | HTTP keepalive 需请求边界和连接生命周期改造；错误风暴需有界队列及丢弃统计；reason phrase 单独作为协议修正处理，本轮均未改变 |

已实施修改的具体边界：

- SSE 的根因是旧游标只在找到完整帧时前移，找不到分隔符时下一次扫描仍从帧首开始。现在同一批输入的工作量随字节数及分片数线性增长，缓冲压缩同步移动两个位置。所有八处调用统一更新，覆盖 Chat / Anthropic 的流式、非流式汇总、完整字节解析和 Native HTTP/SSE 到 WS 的降级路径。保留 LF/CRLF 优先顺序、UTF-8 原始字节、未结束尾帧、容量限制、事件顺序和异常处理。额外状态仅为一个扫描位置。
- 计时的根因是异步观察任务使用解析当时的时钟，且最终记录等待观察者退出后才计算总耗时。现在首内容使用已写出分片的时间，完整耗时在第一次请求结束时固定。异步解析、usage 补全、失败标记、隐私过滤、队列满时降级和恰好一次写入均保留；启用日志时每个观察队列元素增加一个时间戳，受原有队列上限约束。回归测试人为延后观察者 20 ms，验证该延迟不会进入这两个指标。
- 未拆分大型路由模块、引入新依赖或新缓存，也未删除仍承担兼容校验的重复转换代码。当前单文件包含大量协议状态和测试，后续可以独立搬迁测试以便阅读，但这种整理本身没有延迟收益。

指标与基准：

- `ttft_ms` / `upstream_first_byte_ms` 继续表示上游首次数据，可能只是控制事件或心跳。`downstream_first_content_ms` 表示下游写出可展示内容，包含正文、工具输入及推理摘要，不代表屏幕绘制。
- 本次基准专门等待客户端收到非空 `response.output_text.delta`，排除响应头、`response.created`、心跳和空 delta。非流式首内容等于完整响应到达时间。没有将这两个指标冒充真实供应商或 Codex renderer paint TTFT。
- 环境：Apple M4 / macOS 27.0 arm64 / rustc 1.96.0。基线生产代码来自 `a5a3a65a08a97860abcdf7c31d734db47ee4fab7`；先编译带相同基准的优化前二进制，再编译优化后二进制，两者交替执行。使用 release 优化、关闭 LTO、16 个 codegen units，两侧构建参数一致；运行期间不同时执行编译或其他本任务压测。
- 四组端到端场景为 Native Responses SSE、Chat SSE、Anthropic SSE、Chat 非流式 JSON；每组覆盖 1/8/32/64 并发，4 次预热后每个 worker 连续发 8 次多轮请求。模拟上游在文本前等待 10 ms，在结束前再等待 10 ms。三轮合计每侧 10,080 次计入统计的请求，错误均为 0。
- CPU 是当前进程每批请求的用户态与内核态 CPU 时间，包含 mock 上游和客户端；RSS 是该进程整轮运行的累计峰值，不能解释为单请求或仅路由的内存。日志在性能基准中关闭；开启日志后的语义通过日志和观察队列回归测试验证。本机 mock 每次关闭上游连接，连接池与 WS 复用由独立正确性测试验证，没有在该基准中测得握手收益。
- 下列数值为三轮各自分位数/指标的中位数，原始各轮 P50/P95、吞吐、CPU、RSS 和错误数全部保存在 `backend/benches/local_router_results.json`，不能解释为所有样本合并后的分位数。

分帧微基准：同一个事件按 256 字节依次输入，每个大小运行 8 次。此表只测共享分帧器，不包含网络、模型生成或 UI。

| 事件大小 | 优化前 P50 | 优化后 P50 | 分帧加速比 |
| --- | --- | --- | --- |
| 16 KiB | 0.356 ms | 0.012 ms | 29.2× |
| 128 KiB | 21.879 ms | 0.094 ms | 233.6× |
| 512 KiB | 353.056 ms | 0.367 ms | 962.1× |

普通短回复的 64 并发对比，箭头均为优化前 → 优化后：

| 场景 | 首正文 P50 ms | 完整 P50 ms | 成功请求/秒 | CPU ms / 512 请求 | 整轮峰值 RSS MiB |
| --- | --- | --- | --- | --- | --- |
| openaiResponses SSE | 16.57 → 17.00 | 30.26 → 29.88 | 2130 → 2134 | 172.3 → 161.7 | 31.03 → 30.75 |
| openaiChatCompletions SSE | 16.90 → 16.86 | 30.00 → 30.50 | 2125 → 2100 | 221.2 → 217.9 | 32.58 → 32.14 |
| anthropicMessages SSE | 16.33 → 17.10 | 29.70 → 30.49 | 2133 → 2090 | 224.0 → 225.7 | 33.02 → 32.56 |
| openaiChatCompletions JSON | 29.47 → 29.43 | 29.47 → 29.43 | 2161 → 2170 | 166.1 → 162.3 | 33.03 → 32.64 |

普通短回复没有稳定的端到端加速。三轮中 Anthropic 首正文中位数有小幅增加，因此额外执行 10 轮成对的 64 并发复测，每侧再测 5,120 请求，仍无错误：首正文 P50 16.975 → 17.159 ms，P95 18.790 → 18.690 ms；完整 P50 30.531 → 30.550 ms；吞吐 2084 → 2088 请求/秒；CPU 231.602 → 232.251 ms；RSS 30.148 → 30.273 MiB。复测没有显示与大帧收益同量级的短回复变化，也不足以声称短回复更快。主要已验证收益是消除大分片事件的重复 CPU 扫描，预计能减少这类响应对并发请求的调度影响，后者仍需真实负载测量。

复现命令：

```sh
cargo test -p codey --lib local_router::tests -- --test-threads=4
cargo test -p codey --lib route_request_log::tests
cargo test -p codey --lib latency_bench --release --config profile.release.lto=false --config profile.release.codegen-units=16 -- --ignored --nocapture --test-threads=1
CODEY_BENCH_PROTOCOL=anthropicMessages CODEY_BENCH_CONCURRENCY=64 cargo test -p codey --lib loopback_latency --release --config profile.release.lto=false --config profile.release.codegen-units=16 -- --ignored --nocapture --test-threads=1
pnpm test:js
pnpm check
```

验证结果：优化前 150 项路由测试通过；优化后 153 项路由测试、31 项请求日志测试、349 项 JS 测试及 TypeScript/启动补丁检查通过。新增检查覆盖逐字节/不规则分片、中文和多字节字符、CRLF/LF、超过 64 KiB 后压缩缓冲、尾帧延续、上游空闲超时、下游背压超时和读端关闭。既有测试继续覆盖非流式汇总、错标 Content-Type 的渐进输出、鉴权和路由错误、zstd、函数/自定义/命名空间工具、多轮 continuation、上游 WS 复用/退避/降级、发送后中断不重放，以及日志队列满时降级和用量补全。超时检查采用虚拟时钟，保留 Tokio 的毫秒取整容差。格式检查与 `git diff --check` 通过。

未解决的测量与兼容性边界：真实供应商推理、TLS/代理网络质量、原生 renderer 从事件收到至绘制的延迟、错误风暴、开启日志的长期吞吐和稳定 RSS、首字前立即取消仍未由此基准证明。下一步优先在可控的真实供应商请求上关联路由阶段指标与 renderer trace，再决定取消传播、错误/终态契约修正和流式收尾去重。只有明确测得本地握手或日志争用占比后，再考虑下游 keepalive 或错误日志队列；不通过缩短超时、扩大重试、预先发送伪文本或缓存模型回复改变既有业务行为。

### 本地路由稳定性跟进（2026-09-05）

在前述审查基础上，进一步完成 WebSocket 取消传播、截断识别和终态去重。生产代码只修改 `backend/src/local_router.rs`，沿用既有请求内存预算、日志、超时和单请求串行处理机制，没有新增依赖、后台读取任务或重试策略。新增回归集中在 `backend/src/local_router_stability_tests.rs`，不会进入发布构建。前节问题表的第 1 项已完成 WebSocket 部分，第 4 项已按下述兼容边界完成，第 5 项已完成重复终态修正；Anthropic HTTP 状态映射仍保留原契约。

| 优先级 | 问题表现与根因 | 修改与收益 | 影响及风险 |
| --- | --- | --- | --- |
| 1 | 一次 WS 请求独占等待上游，期间未读取下游 Close/Ping；用户关闭后连接名额继续占用 | 复用原有下游对象，在同一任务中同时等待上游和下游控制帧；Close 退出代理并释放上游，Ping 及时回复 | 覆盖原生上游 WS 握手、发送、读事件，以及三种协议的 HTTP 响应头、嗅探、流、完整 JSON 和错误体等待；不在已发送请求后降级或重放 |
| 2 | Chat/Anthropic 内容流在无任何完成依据的 EOF 后，仍被组装为成功 | 在共享累积器收尾处校验已有完成状态，所有汇总和流式调用统一生效 | 保留 `[DONE]` / `message_stop`，也继续接受非空 `finish_reason` / `stop_reason` 后的 EOF；末尾未补空行和 usage 保留。仅内容 EOF 改为既有协议错误路径，避免误报成功 |
| 3 | 适配器发出失败后继续返回 Err，外层再次发送失败；完成写入后的异常也可能触发第二个终态 | 适配状态和 WS 传输层记录终态是否已尝试，每个新消息重置；原始错误继续传播和记录 | completed / incomplete / failed 最多尝试一次，包含终态写入失败。无效消息不会影响下一次请求，取消记为 cancelled，上游故障记为 failed |

取消处理边界：

- 下游 WS 的后续应用消息按原顺序暂存，最多 8 条并计入共享请求内存预算；预算不足时最多暂存一个已受帧大小限制的消息，随后停止读取。出队或取消时释放预算，实际处理仍沿用原有预算校验。缓存上游心跳确认优先于后续请求，Ping 不重建已经开始的上游等待或其超时。
- 队列满后，排在其后的 Close/Ping 仍可能等待当前请求推进。这是有界缓冲和顺序处理的已知限制；不能宣称任意排队压力下取消都立即完成。
- HTTP `shutdown(Write)` 表示客户端不再发送数据，但仍可继续读取响应。TCP FIN 无法可靠区分合法半关闭和取消，因此没有增加 EOF 取消、轮询或提前发送响应头。HTTP 取消仍依赖后续写入失败或原有超时。已通过原始 TCP 半关闭后仍收到完整响应的回归。
- 本轮释放的是本地连接和等待资源；供应商收到断连后是否立即停止推理或计费，需要供应商侧验证。没有修改请求字段、模型选择、工具执行、多轮上下文、超时长度和错误码映射。

验证：新增 12 项回归覆盖两种适配协议 × JSON/SSE/WS × 完成/截断的 12 个组合，原生 WS 取消且不重放，三种 HTTP 上游协议 × 响应头/嗅探/正文流/错误体/JSON 体的 15 个取消等待点，Ping/Pong、原超时保持、队列顺序/数量/共享预算释放、取消日志、连续无效消息后的有效请求、终态去重和 HTTP 半关闭。修改前已复现无完成依据 EOF 被接受和适配 WS 重复终态两个失败，修改后均通过。完整检查为 176 项路由及关联测试（另 2 项基准默认忽略）、31 项日志测试、349 项 JavaScript 测试、`pnpm check`、相关 Rust 格式检查及 `git diff --check`，均通过。

队列数量与共享预算回归测试将 9 条 WebSocket 消息批量缓冲后统一 flush，并使用 500 ms 的上游等待时间，减少逐条小包发送和测试调度对断言的影响；仍严格检查预算不足时暂存 1 条、预算充足时最多暂存 8 条，以及清空队列后预算完全释放。

取消测量使用 release 构建，在 16 个场景中连续执行三轮，共 48 次取消；全部通过，且均收到及时 Pong 和关闭确认，没有额外失败终态。从客户端发起 Close 到 mock 观察到上游连接释放，P50 为 0.108 ms，P95 为 0.123 ms，最大 0.142 ms，包含本地任务调度时间。优化前未单独测得取消延迟，因此不提供虚构的前后加速比；代码审查确认旧实现会继续等待上游推进或超时。

性能对比以首轮优化后的二进制为本轮基线，两侧继续采用相同 release 参数和相同 HTTP 下游 mock。先交替执行三轮 1/8/32/64 并发，再做 10 轮 64 并发复测，偶数轮交换执行顺序。每侧累计 30,560 次计入性能统计的请求，错误均为 0。以下为复测中各轮指标的中位数；P95 和所有原始样本保存在 `backend/benches/local_router_stability_results.json`。

| 64 并发场景 | 首正文 P50 ms，前 → 后 | 完整 P50 ms，前 → 后 | 成功请求/秒，前 → 后 | CPU ms / 512 请求，前 → 后 | 进程峰值 RSS MiB，前 → 后 |
| --- | --- | --- | --- | --- | --- |
| Native SSE | 16.983 → 16.683 | 30.343 → 30.619 | 2106 → 2080 | 165.623 → 167.268 | 28.820 → 28.992 |
| Chat SSE | 16.617 → 16.790 | 30.538 → 30.840 | 2098 → 2069 | 209.340 → 207.487 | 30.578 → 30.727 |
| Anthropic SSE | 16.637 → 16.775 | 30.895 → 30.554 | 2064 → 2091 | 211.505 → 211.873 | 31.117 → 31.281 |
| Chat JSON | 30.176 → 30.457 | 30.176 → 30.457 | 2107 → 2084 | 164.826 → 161.623 | 31.133 → 31.305 |

首正文和总耗时仍有小幅双向变化；复测吞吐差异约 -1.4% 至 +1.3%，CPU 约 -1.9% 至 +1.0%，RSS 增加约 0.15–0.17 MiB。这些结果不能证明普通对话更快，也不能证明严格零开销；主要确定收益是提前释放取消请求的本地资源、避免错误成功和重复终态。该基准没有测量 WS 正常转发吞吐，取消结果也不代表真实供应商推理停止或页面首字。

复现新增检查：

```sh
cargo test -p codey --lib local_router -- --test-threads=4
cargo test -p codey --lib stability_tests -- --nocapture --test-threads=1
```

仍待推进：首先采集真实供应商与客户端绘制的分阶段指标，并补充 WS 持续高并发和开启日志的长时间压测；其次用长文本/并行工具逐事件对照验证适配流收尾重复构造的简化。HTTP 应用层取消、下游连接复用、错误日志有界队列及 Anthropic 状态码统一仍需要单独设计与兼容性验证，本轮未擅自调整。

### 长回复收尾内存优化（2026-09-05）

本轮完成前述收尾去重中的低风险部分：减少 Chat / Anthropic 流式结束时的临时文本副本，并提前释放临时转换结果。生产改动限定在两个流式函数，没有引入依赖、配置项或第二套工具校验。基线为 `f396fabc25633a37fba7cd49c2a8b4a7c48a60ad`；测试和原始数据分别位于 `backend/src/local_router_tail_tests.rs`、`backend/benches/local_router_tail_results.json`。

问题、修改与兼容边界：

- 流式状态已保存实际输出，但收尾又把累积器构造为完整 Chat/Anthropic 对象，再转换为完整 Responses 对象，最后只读取用量和结束原因。现在先释放 Chat 累积器的正文和拒绝文本；Anthropic 累积器只剔除已知可忽略的 text/refusal/thinking/redacted_thinking 块。工具块及未知块继续按原顺序进入完整转换，保留 JSON 参数、对象类型、工具名称和结束状态的校验及异常顺序。
- 结束原因原先借用临时 Responses 对象中的字符串，使整份临时结果留到终态写出。现在仅复制短结束原因，及时释放临时 Responses，以及 Anthropic 的临时 message，降低收尾发送期间的峰值内存。用量映射、错误返回、日志、工具执行、多轮历史和非流式 JSON 路径保持原有处理。
- 没有删除累积器或合并协议状态机：流式过程中仍保存两份状态，工具参数也仍经过原有完整转换。Anthropic 普通函数的最终 JSON 校验与流输出状态不同，Chat 的旧式 function_call 与 tool_calls 聚合也有兼容细节；直接共用一份状态会扩大风险。后续只有确认这部分成本仍显著时，再单独统一校验。

验证使用两种协议各 9 个场景：长文本、8 个并行函数、namespace/custom/tool_search 混合、普通函数非法 JSON、自定义参数错误、tool_search 非对象参数、拒绝、长度限制和思考块。归一化随机 ID 和创建时间后，修改前后约 8 MB 的完整事件记录逐字节相同，正文、参数、用量、事件顺序、失败和 incomplete 终态均参与比较。固定 SHA-256 已加入可运行回归；设置 `CODEY_TAIL_SNAPSHOT` 可以导出完整记录排查差异。检查通过：177 项路由及关联测试（另 3 项基准默认忽略）、31 项日志测试、350 项 JavaScript 测试、`pnpm check`、相关 Rust 格式检查与 `git diff --check`。

性能方法：Apple M4 / macOS 27.0 arm64 / rustc 1.96.0；两侧均为 release、关闭 LTO、16 个 codegen units。五轮成对运行，偶数轮交换顺序；每个场景和并发档位使用独立进程，1 次预热后每个 worker 连续请求 8 次。覆盖 1/8 并发、两种协议和三类负载，每侧共 2,160 次计入统计的请求，错误为 0。长文本为 1 MiB；混合负载额外包含三类工具各 16 KiB 参数；纯工具负载为 8 个函数各 64 KiB 参数。上游按 8 KiB 文本和 4 KiB 参数分片发送，不模拟推理等待；下游为真实回环 HTTP/SSE，日志关闭。

下表为五轮各指标的中位数，均为优化前 → 后。首内容包含正文或工具增量；CPU、RSS 包含路由、mock 和客户端，不能当作路由独占资源。P95、末个增量至终态时间、1 并发结果及各轮样本均保存在原始数据中。

| 8 并发场景 | 首内容 P50 ms | 完整 P50 ms | 请求/秒 | CPU ms / 64 请求 | 峰值 RSS MiB |
| --- | --- | --- | --- | --- | --- |
| Chat 长文本 | 11.341 → 11.550 | 42.310 → 41.697 | 182.7 → 186.7 | 1267.2 → 1224.5 | 162.33 → 137.88 |
| Chat 混合 | 13.140 → 11.752 | 43.268 → 42.157 | 179.2 → 178.8 | 1294.2 → 1271.8 | 250.52 → 229.42 |
| Chat 纯工具 | 7.007 → 5.893 | 19.938 → 19.647 | 391.1 → 399.4 | 585.5 → 575.9 | 70.58 → 67.06 |
| Anthropic 长文本 | 10.650 → 12.775 | 42.138 → 39.142 | 178.0 → 194.6 | 1270.0 → 1186.0 | 174.25 → 137.28 |
| Anthropic 混合 | 11.856 → 12.035 | 43.885 → 42.750 | 174.1 → 179.0 | 1321.2 → 1277.8 | 251.72 → 223.30 |
| Anthropic 纯工具 | 6.081 → 6.467 | 19.384 → 20.996 | 398.7 → 381.9 | 581.0 → 597.1 | 68.67 → 64.81 |

没有忽略不利结果：针对 Anthropic 长文本首内容增加和纯工具总耗时增加，再执行 10 轮成对复测，每侧额外 1,280 请求，错误仍为 0。长文本首内容 11.728 → 11.792 ms，完整 40.814 → 39.680 ms，CPU 1231.021 → 1197.345 ms，RSS 166.258 → 141.336 MiB；纯工具首内容 6.237 → 6.421 ms，完整 20.099 → 20.004 ms，CPU 592.792 → 594.936 ms，RSS 72.508 → 64.352 MiB。初始矩阵和复测均保留，不合并为新的分位数。

可确认的主要收益是降低本地长回复收尾内存峰值：五轮 8 并发长文本下降约 15%–21%，混合负载下降约 8%–11%；长文本总耗时和 CPU 有小幅改善。纯工具速度变化较小且有双向波动，首内容没有稳定加速，不能宣称真实对话 TTFT 改善。两组性能验证合计每侧 3,440 请求、零错误，也不能替代真实供应商、页面绘制、开启日志、长期 RSS 和持续高并发验证。

复现示例：

```sh
CODEY_TAIL_SNAPSHOT=/tmp/codey-tail.json cargo test -p codey --lib long_response_tail_preserves
CODEY_BENCH_PROTOCOL=anthropicMessages CODEY_BENCH_CASE=text CODEY_BENCH_CONCURRENCY=8 cargo test -p codey --lib long_response_tail_latency --release --config profile.release.lto=false --config profile.release.codegen-units=16 -- --ignored --nocapture --test-threads=1
```

`CODEY_BENCH_CASE` 还支持 `mixed`、`parallel_tools`。仍未处理的错误日志有界队列、HTTP 显式取消/连接复用、Anthropic 状态码统一和真实客户端性能采集，继续按前节边界推进；本轮不将这些改造混入文本收尾优化。

### 控制台与页面注入

cdp.rs 负责准备嵌入资源、安装桥接、首次注入和健康复核。src/overlay.tsx 挂载 React 控制台；public/ 中的脚本分别处理模型、插件、会话、提示词和平台增强。

控制台首次打开时再加载完整界面。轻量健康探针持续确认桥接状态，只有确定桥接缺失时才重注入；页面忙或探测超时保持保守状态。

用户脚本在同一文档成功执行后不会因桥接恢复而重复运行，失败可重试，新文档正常运行。内置脚本保留自身的恢复逻辑。桥接安装检查 Runtime.evaluate 的 exceptionDetails；失败释放新连接，成功替换后关闭旧 pump。new-document 注册随各次 CDP 会话维护，不使用跨连接 target 缓存。

模型列表热更新通过 QueryClient 发布新结果，不直接修改 React 共享查询对象，确保已挂载的对话模型选择器收到通知。WebSocket 和原生网页搜索线路的模型集合变化仍需重启生效。

Codex 更新后，优先检查启动补丁、app-server 参数结构、入口资源和页面语义选择器。兼容判断必须唯一命中，不能用宽泛文本或 DOM 位置猜测。

### 会话与插件

启动期会话维护只在受控 Codex 停止后修改 rollout、SQLite 和索引。运行中的导入、导出、删除轮次与恢复备份会先释放目标会话，再使用临时文件、大小限制、原子替换和并发校验；无法稳定确认轮次或当前页面已切离时拒绝删除。

插件市场修复使用随程序分发的快照和回滚替换。状态读取保持只读，只有用户触发修复时才更新 Codey 管理的市场目录与注册项。

Computer Use 沿用 Codex 管理的 `unified-computer-use` 插件及其 `cua_repl` 服务。新版桌面端可能自动关闭旧 `computer-use` MCP，不能仅据此认定电脑操作不可用，应验证官方统一入口。Codey 不再创建 `codey_computer_use`，也不重写旧服务或插件开关；本地路由与原生线路启动均保留原配置。回归测试覆盖旧服务开启、关闭以及两种启动方式，验证磁盘配置和退出恢复保持原样，运行参数不新增 MCP 或插件覆盖。此前生成的重复条目可在确认官方入口可用后从用户配置及 Codey 保存的配置中移除。

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

请求日志清空与重启的功能回归显式使用 10 秒写入线程停止期限，为 CI 文件 I/O 和线程调度留出余量；生产配置仍默认 1.5 秒，停止超时另有独立测试。清空断言失败时输出完整结果，以区分停止超时、文件删除失败和重启失败。

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
