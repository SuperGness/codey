# Codey 内部开发文档

本文档面向开发和维护人员，只保留当前架构、开发流程、关键边界和已知限制。用户可见功能维护在 README.md；历史方案和逐版本改动由 Git 记录，不在这里累积。

## 核心设计

- Codey 是 Rust 桌面辅助进程，负责启动、监控和停止官方 Codex Electron 客户端。
- 配置界面由 React 实现，构建后嵌入 Codey，并通过 CDP 注入 Codex 页面；通常没有独立常驻配置窗口。
- 本地路由开启时，Codex 只连接本次启动的回环网关。官方账号沿用 Codex 登录，第三方线路使用 Codey 保存的凭据。
- 线路、模型和上游格式分别识别。模型选择器使用带线路信息的稳定 ID，只有本地路由边界会把它还原为供应商原始模型 ID。
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
5. 启动 Codex，确认 app-server 收到完整运行时覆盖，再通过 CDP 安装桥接与页面增强。macOS 的 `CODEX_CLI_PATH` 指向私有可执行包装脚本，由脚本恢复可能被 Codex 子进程过滤的兼容环境后再进入 Codey CLI 包装分支，禁止把完整 Codey 桌面入口直接暴露为 CLI。当前 Codex 未开放主进程 Inspector 时，以已确认的 CLI 兼容入口继续运行并标记为非致命降级；只有启动补丁与兼容入口都失败时才停止 Codex。
6. 启动健康检查、退出监听、通知和平台保护任务。设置保存后，支持热更新的项目立即替换；影响启动参数、角色集合或能力目录的项目标记为需要重启。
7. Codex 退出、系统信号或安装更新时，先停止 watcher 和路由，再停止受控 Codex，最后清理 Hook、租约及其他 Codey 自有运行状态。

启动任一步失败都应走同一清理路径。会话数据的安全修复不会在退出时回滚；临时路由、Hook 和运行文件必须可恢复。

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

local_router.rs 维护不可变线路快照，按明确线路元数据、带线路的模型 ID 和可信会话绑定解析请求。保存线路后只影响新请求，已有流继续使用原快照。

本地路由负责官方与第三方认证隔离、模型 ID 还原、流式转发和上游格式适配。图片生成请求按明确线路元数据、会话绑定或全局默认模型的顺序选择线路，并只向 OpenAI 兼容上游透明转发 Images API，不尝试转换为 Anthropic Messages。WebSocket 能力由共享 Provider 开启，再通过模型目录的 `prefer_websockets` 按线路选择；支持的线路使用 WebSocket，不支持的线路继续使用 SSE。第三方线路的网页搜索、自动审核和远程压缩能力只有在配置与模型目录都能确认时才启用。本地路由关闭时，不安装路由覆盖，只过滤当前 Codex Provider 的可用模型。

Codex 自动标题优先使用可用官方账号的 `gpt-5.6-luna`；没有官方账号时，依次使用当前默认第三方线路的同名模型和当前默认模型，推理强度保持 `low`，请求失败则保留客户端临时标题。

路由层不做跨供应商故障转移、轮询或自动重试。长连接只允许在请求尚未发送时回到同一线路的普通流式请求；发送后失败直接返回。

### 控制台与页面注入

cdp.rs 负责准备嵌入资源、安装桥接、首次注入和健康复核。src/overlay.tsx 挂载 React 控制台；public/ 中的脚本分别处理模型、插件、会话、提示词和平台增强。

控制台首次打开时再加载完整界面。轻量健康探针持续确认桥接状态，只有确定桥接缺失时才重注入；页面忙或探测超时保持保守状态。

Codex 更新后，优先检查启动补丁、app-server 参数结构、入口资源和页面语义选择器。兼容判断必须唯一命中，不能用宽泛文本或 DOM 位置猜测。

### 会话与插件

启动期会话维护只在受控 Codex 停止后修改 rollout、SQLite 和索引。运行中的导入、导出、删除轮次与恢复备份会先释放目标会话，再使用临时文件、大小限制、原子替换和并发校验；无法稳定确认轮次或当前页面已切离时拒绝删除。

插件市场修复使用随程序分发的快照和回滚替换。状态读取保持只读，只有用户触发修复时才更新 Codey 管理的市场目录与注册项。

### 提示词、子代理与 FastCtx

提示词优化可使用运行中的 Codey 路由，也可使用独立配置。地址、认证和模型由后端校验；日志不保存提示词正文或凭据。

子代理增强只在原生 macOS 和 Windows 启用。五个用户角色与内部 default 角色的配置源位于 backend/src/codex_config_guidance.rs，默认规则数据位于 backend/resources/subagent-rules.default.json。当前路径直接使用 Codex 原生 agents 工具和生命周期 Hook，不再使用旧版 sidecar、逐任务回执、prepare_delegation 或 resolve_batch 流程。只读任务最多并行三个；出现写入角色时最多两个，并由根代理在所有尝试结束后验收结果。活动 attempt 全部通过绑定、marker 和 `files.read` 能力校验时，可信根 turn 可继续使用规则确认的本地读取、网页检索、MCP Resource 与数据库 schema/只读 SQL 工具；SQL 只接受单条、可保守证明为只读的语句，写入、命令、视觉、未知工具以及 writer/mixed/unverified 批次仍保持关闭。角色名、词法 SQL 校验和 Hook 不是最终安全边界，真实权限仍由数据库只读账号以及 Codex sandbox 与 approval 设置决定。

完整且无筛选的 agents 列表若只包含根代理，会精准回收从未绑定、从未启动的 pending spawn，覆盖 provider 在线程上限等失败后缺少 PostToolUse 回执的路径；已绑定或已启动 attempt 不受影响。顶层 wait 超时和仅根代理快照不算语义进展，不得重置 Stop 的 10 分钟停滞恢复窗口，只有带具体代理身份的状态或输出变化才会重置。

视觉角色由原生任务胶囊授予 `visual.inspect`，受信的图像、截图、CUA 和 `open_in_codex` 工具只对视觉角色开放。Responses 工具结果中的图像在 Chat Completions 与 Anthropic 回退协议中会转换为紧随 tool result 的用户图像块，不能退化成 base64 JSON 文本。协作响应中的解密失败、空 payload 或任务体缺失统一触发一次活动代理任务重述恢复；Codey 不尝试本地解密 provider 载荷。

内置 FastCtx 只提供文件读取、搜索、发现和批量替换。检测到用户已有 FastCtx 时不重复注册；内置版本通过本次进程覆盖加载，不写入用户 Codex 配置。版本与固定提交以 Cargo.toml 和 THIRD_PARTY_NOTICES.md 为准。

### 通知与诊断

通知支持飞书、企业微信、Telegram 和微信 ClawBot，最多保存 32 个渠道。完成、失败和等待介入事件由真实任务状态触发；不确定是否已送达时不盲目重发。

Trace 与 Crashpad 保护只处理各自允许范围内的诊断数据，不触碰会话和账号数据。Windows 性能保护在启动补丁中启用，并通过运行状态自检展示是否生效。

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
