# Codey 构建/性能/子代理流程审计（2026-09-07）

范围：Cargo 工作区（backend、vendor/CodeyRuntime）、Vite/React overlay、注入脚本、scripts/、.github/workflows、子代理门禁 Hook 流程。方法：先建基线，再用编译器与临时计数探针核实，只落地低风险且可验证的改动。

## 基线与对比

| 指标 | 优化前 | 优化后 |
| --- | --- | --- |
| pnpm run check | 2s | 2s |
| pnpm run test:js | 3s，365 项（364 通过 1 跳过） | 3s，365 项（364 通过 1 跳过） |
| pnpm run vite:build | 3s；codey-overlay.js 975 KB（gzip 249 KB），inject 186 KB | 不变 |
| cargo test --workspace | 28s | 36s（含重新编译 vendor 改动） |
| cargo clippy --all-targets | 4s | 13s（同上） |
| cargo build --release（半冷） | 163s | 见下方 release 数据 |
| target/release/codey | 19.9 MB | 19.9 MB（不变；被删代码此前已被 LTO 剔除） |
| target/release/codey-fastctx | 42.4 MB | 42.4 MB（不变） |
| pnpm run build 中 Vite 执行次数 | 2 次（build-overlay.mjs + build.rs 再跑一次） | 1 次 |
| 开发增量：touch lib.rs 后 cargo build / cargo test --no-run | 3s / 5s | 不变 |

## 已完成的修改

1. **backend/build.rs + scripts/build.mjs：release 构建不再重复跑 Vite。** build.mjs 已先执行 build-overlay.mjs，再由 cargo 触发 build.rs 时会因为 release profile 独立的 build-script 指纹再跑一遍 `npm run vite:build`。现在 build.mjs 给 cargo 传 `CODEY_SKIP_OVERLAY_BUILD=1`，build.rs 收到后只校验 `dist-overlay/codey-overlay.js` 存在（缺失则给出明确报错），否则维持原逻辑。直接 `cargo build/test` 的行为不变；include_str! 依然把产物登记到 dep-info，产物变化仍会触发重编译。
2. **vendor/CodeyRuntime/crates/codey-runtime-core：删除编译器确认无引用的函数（-617 行）。** 方法：把候选 `pub fn` 临时改为 `pub(crate)` 后 `cargo check --workspace --all-targets`，只删除 rustc 报 `never used` 的项，再级联复查一次。删除项：ports.rs 的回环端口守卫锁链（`acquire_resilient_loopback_port_guard*`、`LoopbackPortGuard`、`launcher/manager_guard_port`、`can_connect_loopback_port`、`acquire_loopback_port_guard`）及其 11 个只服务于这些函数的测试；bridge.rs 的 `run_periodic_evaluations`、`add_script_to_new_documents`、`CdpSession::detach`、`runtime_evaluate_result_is_false`；app_paths.rs 的 `user_data_candidates*`、`resolve_codex_runtime_version`、`codex_runtime_version`、`parse_codex_runtime_version`、运行时版本缓存；codex_sqlite.rs 的 `codex_session_db_path*`、`relative_to_codex_home`、`sqlite_has_table`；config_manager.rs 的 `restore_latest_backup`、`set_provider_wire_api`；paths.rs 的 `set_settings_path_for_tests`；plugin_marketplace.rs 的 `preserve_openai_curated_remote_marketplace_config`、`merge_marketplace_configs_into_text`。ports.rs 为保留的 `select_packaged_codex_debug_port_with` 补了 2 个行为测试。backend 对 ports.rs 的唯一引用（launcher.rs 的 `select_packaged_codex_debug_port`）保持不变。
3. **INTERNAL_DEVELOPMENT.md** 记录跳过标志与 vendor 清理。

## 子代理流程：实测与结论

Hook 架构：Codex 每个事件（PreToolUse/PostToolUse/UserPromptSubmit/SubagentStart/SubagentStop/Stop/SessionEnd）拉起一次 `codey --codey-subagent-gate-hook`，读 stdin JSON，取 state_root 级文件锁，按事件读写会话目录下的账本、marker、attestation 与状态文件，再输出决定。

用临时计数器（已还原）测得每事件的账本 open / save / 规则加载：

| 事件 | ledger open | ledger save | rules load |
| --- | --- | --- | --- |
| spawn PreToolUse | 2 | 1 | 1 |
| spawn PostToolUse | 1 | 1 | 0 |
| wait_agent PostToolUse（2 活动，超时） | 4 | 0 | 0 |
| wait_agent PostToolUse（1 完成） | 4 | 1 | 0 |
| root read_file PreToolUse（批次中） | 1 | 0 | 0 |
| Stop（1 活动） | 3 | 0 | 0 |
| list_agents PostToolUse（全终态） | 2 | 1 | 0 |

每次 open = create_dir_all + 打开锁文件 + try_lock + 清理临时文件 read_dir + 读文件 + JSON 解析 + migrate 校验。这印证了前一轮审查的 P1（同一事件重复加载）。**本轮未改**：维护者在 output/subagent-review-2026-09-07-verification.md 中明确要求该重构（引入每事件上下文对象、改变持久化边界）与正确性修复分开做，且 6000 行门禁没有并发压测基准可证明收益；强行合并 open 会触碰 fail-closed、fence 与 marker 一致性语义。并行度（只读 3 / 有写 2）、失败重试与结果汇总路径未发现职责重复或可安全并行的串行步骤：门禁本身不派发任务，派发由 Codex 原生 agents 工具完成，Hook 只做准入与对账。

建议的后续（按收益排序）：
- P1：在 `handle_hook_for_runtime_at` 取锁后构造一次 `{store, ledger, rules}` 贯穿传递，结束时一次 save；用上表作为回归断言（wait 路径 4→1）。
- 把 attestation 的 transcript 读取移出全局锁（M6）。
- 会话目录 GC 需要可靠活动证据，暂不做。

## 性能审查结论（未改动项及原因）

- **codey-fastctx 42 MB**：~32 MB 是 bpe-openai 的 o200k 词典；fastctx 上游只有 `pdf` feature，ratatui/clap/image/rayon 均无条件依赖，`default-features = false` 已是最小。Codey 侧无法裁减；需上游加 feature 门或压缩词典。
- **codey_lib 直接依赖 fastctx**（codex_config.rs 引用 3 个配置辅助函数），使主二进制与 lib 测试都要链接 fastctx 依赖图，违背拆 sidecar 的初衷。可将 3 个函数本地化，但需逐字对照上游保证行为一致，本轮未做。
- **Mantine 全量 styles.css（273 KB）内联进 overlay**：仅使用约 10 个组件，改按组件引入 CSS 预计减 150–190 KB，但涉及 Select/Combobox/Popover 等依赖样式，需要人工视觉验证，未改。
- overlay 已按需加载（首次打开控制台时注入），注入脚本已 esbuild 压缩并校验占位符。
- cargo-machete、grep 复核：chrono、qrcode、zstd、semver、hyper-util 等依赖均有使用。`cargo tree -d` 的重复版本全部来自 rmcp/schemars/reqwest/image 的传递依赖，工作区无法对齐。
- CI：`build-desktop.yml` 在 3 个 runner 上重复执行 ci.yml 已跑过的 test/clippy；去掉可省每 runner 一次 debug 全量编译，但会削弱发布门禁，属流程决策，未改。
- Release profile fat LTO + codegen-units=1 是编译慢的主因，也是能剥掉两份多余词典的原因；Windows 已用 thin LTO 覆盖。

## 无法确认、需人工决定

- vendor `windows_open_url`、`windows_activate_process_window`、`windows_apply_codey_icon_to_process_window`、`windows_process_control_strategy`：backend 无引用，但 `cargo check --target x86_64-pc-windows-msvc` 在本机因 ring/libsqlite3-sys 的 C 构建失败无法核实，保留。
- 前端 `App.types.ts`、`components/mantine/index.tsx` 等导出的 Props/类型只在本文件使用（tsc 不报未用导出）；属公开类型面，未删。
- 动态规则文件 `subagent-rules-v1.json` 读取链无写入方（前一轮 L5），维护者已决定保留。

## 验证命令与结果

pnpm run check ✅；pnpm run test:js ✅ 364 通过 1 跳过；pnpm run vite:build ✅；cargo fmt --all -- --check ✅；cargo test --workspace ✅（lib 1074 通过 3 忽略，vendor-core 56 通过）；cargo clippy --workspace --all-targets -- -D warnings ✅；git diff --check ✅；CODEY_SKIP_CODESIGN=1 pnpm run build ✅（release 全流程 99s，仅 1 次 Vite；基线 163s 含大量依赖重编译，不能直接对比，可比部分是省掉 1 次 Vite（约 3s + node 启动））。
