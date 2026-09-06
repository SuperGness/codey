# Codey 代码审查报告（2026-09-06）

审查范围：backend/src（10.2 万行）、vendor/CodeyRuntime（2.6 万行源码 + 1.3 万行测试）、src/（1.5 万行）、public/（1.1 万行）、scripts、tests、CI。
方法：clippy 附加 lint（too_many_lines / cognitive_complexity / redundant_clone / needless_pass_by_value）、全仓库引用统计、vendor 依赖图分析、函数骨架与热点函数逐段阅读。config.rs / codex_config.rs / model_catalog.rs 等配置模块做了全文通读；local_router、subagent、commands、launcher、日志/会话、前端、注入脚本为骨架 + 热点 + 反模式定向阅读，未逐行通读的部分在置信度中标注。
原则：只报告有代码证据的问题；性能项只能定"嫌疑"，需按给出的方式实测后才能定级；不建议在未确认业务语义前改动核心功能。

---

## 一、高风险 / 高收益

### H1. local_router.rs 单文件 18723 行，10 个函数超过 150 行
- 位置：backend/src/local_router.rs 全文；最大函数 `proxy_parsed_responses_inner` 1864-2327（463 行）、`proxy_upstream_websocket` 7229-7517（288 行）、`handle_responses_websocket` 1452-1694（242 行）、`handle_connection` 959-1191（232 行）、`ResponsesSseState::finish` 11384-11569（185 行）、`proxy_image_generation` 1192-1374、`append_responses_tool_to_chat_tools` 5085-5263、`LocalRouter::start` 213-382、`UsageCapture::observe_byte` 8602-8761、`proxy_native_response_to_websocket` 7925-8077。
- 类型：结构混乱 / 可维护性。
- 描述：clippy 在该文件报出 26 处超长函数告警，占全仓库 too_many_lines 告警的 30%。`proxy_parsed_responses_inner` 内部依次完成：WebSocket 下游预处理、模型默认值回填、路由解析与绑定刷新、请求日志探针标注、协议桥选择、上游 URL/头构造、上游 WebSocket 尝试、HTTP 发送、错误映射、响应写回，共 10 个阶段。
- 影响：任何协议改动都在同一函数内触碰共享可变状态（`body`、`encoded_body`、`headers`），回归面大；文件级 6800 行测试与实现混在同一编译单元，增量编译慢。
- 建议：按现有函数群拆为目录模块，行号范围如下（均为当前文件行号）：
  - `local_router/server.rs`：213-490 生命周期、492-958 RouterServer/RouteBindings/RouterSnapshot/RouteResolver
  - `local_router/http.rs`：5921-6260 请求头/体解析与预算、8088-8400 响应写回
  - `local_router/responses.rs`：959-2360 连接分发与 Responses 主链路，其中 `proxy_parsed_responses_inner` 拆成 `resolve_target`、`build_upstream_request`、`send_upstream`、`relay_response` 四段
  - `local_router/auth.rs`：2362-2900 路由提示、prompt cache key、官方鉴权
  - `local_router/protocol/{mod,chat,anthropic}.rs`：2891-3260 协议枚举与端点；3414-3843 与 4347-5930 Responses→Chat；3845-4346 Responses→Anthropic
  - `local_router/websocket.rs`：6253-7930
  - `local_router/request_log_tap.rs`：8403-9060（含手写 JSON 字节状态机）
  - `local_router/sse/{cursor,anthropic,chat,responses}.rs`：9575-11906
  - 测试拆到各模块 `tests.rs`
- 置信度：高（结构事实）。拆分需先解决 H5 的测试脆弱性，否则 48 个读源码文本的 JS 测试会误报。

### H2. Codey 配置每次保存全量重读解析 4 份 JSON 并重写 3 份备份，备份失败阻断主保存
- 位置：backend/src/config.rs 1976-2024（`ConfigStore::save` → `rotate_valid_backups`）、2032（`parse_config_contents`）。
- 类型：性能 + 潜在逻辑错误。
- 描述：`rotate_valid_backups` 读主文件与 3 份备份，每份经 `parse_config_contents`（serde_json::Value 解析 + CodeyConfig 反序列化 + `normalize()`），去重后对每份 `persist_private_bytes`（临时文件 + fsync + rename + 目录 fsync）。`save` 中 `self.rotate_valid_backups()?` 失败会直接返回错误，主配置不落盘。`commands.rs` 中 `save_config_to_store` 有 20 个调用点，每次设置改动都走这条路径。
- 影响：单次保存最多 4 次 JSON 全量解析 + 4 次 normalize + 8 次 fsync；备份目录不可写或磁盘满时用户无法保存任何设置。
- 建议：备份改为 rename 链，失败降级为日志：
  ```rust
  fn rotate_backups_best_effort(&self) {
      for index in (1..3).rev() {
          let _ = fs::rename(self.backup_path(index), self.backup_path(index + 1));
      }
      if let Err(error) = fs::copy(self.path, self.backup_path(1)) {
          error_log::record_failure("config_backup_failed", ..., error);
      }
  }
  ```
  若必须保证"备份都是合法 JSON"，用长度 + sha256 去重代替全量解析。
- 收益与验证：fsync 从 8 次降到 2 次；用 `strace -e trace=fsync,rename`（macOS 用 `fs_usage`）对比保存一次的系统调用数；`cargo test config::` 中 `config_load_recovers_from_the_newest_valid_backup` 等 3 个用例需保持通过。
- 置信度：高。

### H3. codex_config.rs 约 370 行 TOML hooks 写入链在生产路径从不落盘
- 位置：backend/src/codex_config.rs 2726-3095（`enable_subagent_gate_hooks`、`enable_hooks_feature`、`enable_fastctx_route_hook`、`remove_codey_hooks`、`remove_codey_hook_groups`、`*_is_codey_owned` 系列、`remap_hook_state_entries`、`enable_codey_hooks`、`append_codey_hook`、`codey_hook_table`、`codey_hook_inline_table`）。
- 类型：无用代码 / 冗余逻辑。
- 描述：唯一生产调用链 `apply_isolated_runtime_router_config`(373) → `patch_config_with_fastctx_mode`(1979) → `enable_subagent_optimization`(2030) → `enable_subagent_gate_hooks`(2140)。产物 `effective` 文档只交给 `build_isolated_runtime_overrides`(2342) 抽取 `-c` 覆盖项，该函数对 hooks 只读取 `features.hooks` 布尔值，`[[hooks.PreToolUse]]` 与 `hooks.state.*` 子树整体丢弃；实际 hooks 来自 `build_runtime_hooks_file`(2224) 生成的 hooks.json。`hook_trust_hash` 在两条路径各算一次（2947-2968、2297-2306）。`patch_config` 本身已是 `#[cfg(test)]`（1926）。codex_config/tests.rs 1998-2386 大量断言 `document["hooks"]`，验证的是一条生产不落盘的路径。
- 影响：维护者会误以为 Codex 从 config.toml 读取 hooks；启动多做一遍 TOML 构建与哈希；测试给出虚假安全感。
- 建议：`enable_subagent_optimization` 只保留 `features.hooks = true`；删除 TOML hooks 构建链；tests.rs 相应断言迁移到 `build_runtime_hooks_file` 的输出。`remove_codey_hooks` 若要保留用于清理旧版残留，应在 `repair_persistent_codey_runtime_config`(1257) 中实际调用（当前未调用，说明残留清理其实没有执行）。
- 需确认：是否存在旧版 Codex（Windows 或早期 macOS 包）仍从 config.toml 读取 hooks 的兼容需求。
- 置信度：中高。

### H4. vendor/CodeyRuntime 约 60% 源码与 80% 测试未被 backend 使用，但被整体编译与测试
- 位置：vendor/CodeyRuntime/crates/codey-runtime-core/src、codey-runtime-data/src 及两者 tests/。
- 类型：无用代码 / 模块划分不合理。
- 描述：backend 对 vendor 的全部引用统计为 14 个模块约 40 个符号：windows_integration 封装函数、diagnostic_log、paths、bridge、cdp、model_suffix、config_manager、codex_sqlite、app_paths、plugin_marketplace、launcher（5 个函数）、relay_config（仅 `default_codex_home_dir`，它本身只是转调 `codex_home`）、ports、models；data crate 仅 `delete_local_from_paths`。以下模块无任何 backend 引用：
  | 模块 | 源码行数 | 对应测试行数 |
  |---|---|---|
  | relay_config（除 1 个转调函数） | 2464 | 3211 |
  | settings | 1954 | 176 |
  | computer_use_guard | 1189 | 0 |
  | routes | 704 | 1407（bridge_routes） |
  | zed_remote/* | 1509 | 650 |
  | upstream_worktree/* | 1208 | 395 |
  | stepwise | 531 | 0 |
  | assets（含 3 个注入脚本副本，renderer-inject.js 7444 行） | 531 | 0 |
  | install/* | 789 | 220 |
  | update | 346 | 171 |
  | watcher | 378 | 251 |
  | user_scripts + script_market | 553 | 0 |
  | model_catalog | 804 | 446 |
  | status、native_menu、codex_local_storage、http_client | 559 | 0 |
  | data: provider_sync、markdown、backup | 1671 | 2244 |
  它们仍被编译的原因是 vendor `launcher.rs` 顶部 `use crate::settings::…; use crate::status::…;` 以及 `routes.rs` 引用了 install/model_catalog/stepwise/upstream_worktree/user_scripts/zed_remote。backend 实际使用的 5 个 launcher 函数（`build_codex_command`、`build_packaged_activation`、`build_macos_open_command`、`activate_packaged_app`、`wait_for_windows_process_id`）不触及 settings/status。
- 影响：约 1.6 万行源码 + 1.08 万行测试每次 `cargo test --workspace --locked` 都编译执行；release fat LTO 下编译时间被放大；维护者阅读成本；vendor 侧重复实现（见 L9）与 backend 分叉。
- 建议：分两步。第一步把 5 个被用函数移到 `launcher/commands.rs` 并让 `launcher.rs` 其余部分改为 `#[cfg(feature = "full-launcher")]`；第二步在确认无其他消费者后删除上表模块与测试。`assets/inject/*.js` 三个脚本与 public/ 下同名脚本已完全分叉（public/renderer-inject.js 1203 行 vs vendor 7444 行），应删除 vendor 副本。
- 需确认：vendor/CodeyRuntime 是否同时供其他项目使用。git log 显示该目录只在本仓库内演进（最早提交 2026-07-31，无 subtree/submodule 标记），README 与 INTERNAL_DEVELOPMENT.md 未提及外部消费者。
- 置信度：高（引用事实）；删除时机中。

### H5. 测试体系以源码文本断言为主，构成所有重构的前置障碍
- 位置：tests/*.test.mjs（53 个文件）；backend/src/codex_config/tests.rs。
- 类型：测试质量 / 可维护性。
- 描述：53 个 JS 测试中 48 个通过 `readFileSync` 读取 src/、public/、backend/src 源码文本，共 1162 处 `includes`/`match` 断言；0 个测试直接 `import` src 模块做行为验证（tests/helpers/load-typescript-module.mjs 存在但只被 10 个文件用于加载少量纯函数）。被读取最多的是 src/App.tsx（12 个测试）、backend/src/commands.rs（7 个）、src/FeaturePolicyCard.tsx（6 个）、backend/src/launcher.rs（6 个）。Rust 侧 codex_config/tests.rs 3402 行中有整段断言不落盘文档（见 H3）。
- 影响：H1 拆文件、H2 改保存路径、L12 前端拆组件都会让大量测试因"字符串不再出现在该文件"而失败，却不能捕获真实行为回归；同时 CI 用 348 项 JS 测试通过给出虚假信心。
- 建议：把源码文本断言测试标记为 smoke 层，只保留"注入脚本必须包含某幂等标记"这类确有必要的；对 runtimeStatusPollScheduler、modelIds、routeShortNames、useModelSelection 等纯逻辑模块补 import 级单元测试；Rust 侧把 hooks 相关断言迁到 hooks.json 输出。重构任一文件前先跑 `pnpm run test:js`，用失败列表反查哪些断言是伪依赖。
- 置信度：高。

### H6. 子代理 Hook 每次事件的固定 I/O 成本
- 位置：backend/src/subagent_gate.rs 133-171（入口）、308-435（`handle_hook_for_runtime_at`）；subagent_gate/state.rs 25-62（`HookStateLock`）；subagent/rules.rs 407-490（`load`、`persist_last_good`）；subagent_orchestrator.rs 2085-2379（`authorize_child_tool_with_context`）。
- 类型：性能（架构层）。
- 描述：每个 Codex Hook 事件都启动一个新的 codey 进程，然后：获取全局文件锁（try_lock 自旋 5ms，2 秒超时）；`rules::load` 读取 live 规则文件并再读一次 last-known-good 做字节比对（注释明确说明"进程级缓存无法跨事件复用"）；`LedgerStore::open` + `load` 读会话账本；PreToolUse 路径还会读 attestation、markers 等状态文件；非 allow 结果再写 telemetry。`sql_is_read_only`(2106-2315) 是约 200 行手写 SQL 词法器。
- 影响：高频工具调用（子代理密集读文件）时每次工具调用都叠加进程启动 + 数次文件读 + 锁竞争；`record_hook_evaluation` 只在 deny/block/error 时记录 `latency_ms`，allow 路径无基线数据。
- 建议：短期在 allow 路径按 1/50 采样记录 latency_ms 建立基线；中期评估把规则 + 账本读取合并为一次读（单个状态文件或 SQLite），或让 codey 主进程常驻一个本地 socket 服务由薄 hook 转发。`sql_is_read_only` 建议补充对抗用例：块注释嵌套、`WITH … INSERT`、多语句、`ATTACH`，代码已处理 `/*`、`;`、`ATTACH`、`WITH`，但缺少模糊测试。
- 置信度：中（成本结构是事实，用户可感知程度需实测）。

### H7. 模型别名解析 5 处实现且 split 方向不一致
- 位置：backend/src/model_id.rs 13-26（`historical_source`）、model_catalog.rs 927-934（`split_once('/')`）、codex_config.rs 1617-1624（`rsplit_once('/')`）、config.rs 1229-1232 与 1485-1596（三处前缀匹配）；别名构造在 local_router.rs 939-958（`model_alias`、`encode_alias_component`）；"线路 × 模型 → 别名"循环在 config.rs 1024/1073/1242/1344 与 local_router.rs 606-660 重复 5 次。
- 类型：重复实现 + 潜在逻辑错误。
- 描述：`encode_alias_component` 保证 provider 段不含 `/`，因此 `split_once` 正确；`codex_config::is_route_qualified_model` 用 `rsplit_once`，模型名本身含 `/`（如 `org/model-name`）时得到不同的 provider 段。
- 影响：同一别名在配置修复与路由解析两侧可能得到不同归属，触发"无法确认线路"错误或错误匹配。
- 建议：在 model_id.rs 增加 `RouteAlias { provider_key, upstream }`、`parse_alias(&str) -> Option<RouteAlias>`、`model_alias(provider_id, model) -> String`，把 `ROUTER_PROVIDER_ID`、`CODEX_AUTO_REVIEW_MODEL` 一并移入，打断 config.rs ↔ local_router.rs 的双向依赖；五处循环统一消费 `configured_model_targets()`。
- 验证：为含 `/` 的上游模型名补单元测试，覆盖 `is_route_qualified_model` 与 `route_scoped_upstream_model_id`。
- 置信度：高。

### H8. 原子私有写入有 3 套实现，其中 1 套非原子
- 位置：backend/src/fs_util.rs 83-113（规范实现）；config.rs 2046-2091（`persist_private_bytes` + `write_private_temp`，与 fs_util 逐行等价，额外一次冗余 `set_permissions`）；codex_config/fs_io.rs 21-36（`write_private_file`：truncate 后 write，非原子，用于 `read_or_create_constraint_file`(658) 首次落盘用户可编辑模板）；vendor settings.rs 1061-1078（固定 `.tmp` 名、无 fsync）。
- 类型：重复实现 + 潜在逻辑错误。
- 影响：进程在 truncate 与 write 之间被杀会留下半文件；vendor 版固定临时文件名在两进程并发写时互相覆盖。
- 建议：全部改用 `fs_util::atomic_write_private_with_parent`；删除 config.rs 与 fs_io.rs 的副本。
- 置信度：高。

---

## 二、低风险 / 高收益

### L1. ClawBot iLink 客户端逻辑在命令层与通知层重复
- 位置：backend/src/commands/wechat_claw.rs 1348（`wechat_claw_base_info`）、1438（`ilink_headers`）、1462（`random_wechat_uin`）、1468（`ilink_client_version`）；backend/src/notifications/channels/wechat_claw.rs 26（`ilink_post`）、112、118、128。
- 类型：重复实现。
- 描述：`ilink_client_version`、`wechat_claw_base_info` 逐字相同，`random_wechat_uin` 仅变量名不同；iLink 请求头（AuthorizationType / X-WECHAT-UIN / iLink-App-Id / iLink-App-ClientVersion / Authorization）在两处各写一遍。
- 影响：协议头调整需改两处，容易漏改一侧导致登录与推送行为不一致。
- 建议：抽 `notifications/channels/wechat_claw/ilink.rs`，导出 `ilink_headers(token)` 与 `base_info()`，命令层与渠道层共用。
- 置信度：高。

### L2. config.rs 死字段、恒空迁移路径与重复解析
- 位置：backend/src/config.rs 457-465 与 1256-1265（`RuntimeModelTarget.request_provider_id`、`request_model`：全仓库仅测试引用，值恒为常量/等于 `upstream_model`）；878-929 与 1274-1304（`migrate_provider_default`：`normalize_global_default_model` 无条件 `default_model_by_provider.clear()`，且 `parse_config_contents` 必先 normalize，故迁移永远读到空）；2032-2044（`parse_config_contents` 的 `initialRouteImportCompleted` marker 检测与 `normalize()` 806-808 判断等价，多付一次 `serde_json::Value` 全量解析）；467-575（8 个仅返回常量的 `default_route_request_log_*` 函数）。
- 类型：无用代码 / 冗余逻辑。
- 建议：删除两个字段及 `runtime_gateway_provider_id()`；删除 `migrate_provider_default` 与 `looks_like_empty_default_route` 中 `default_model_by_provider.is_empty()` 条件；`parse_config_contents` 改为 `serde_json::from_str::<CodeyConfig>(contents)?.normalize()`；`RouteRequestLogConfig` 用容器级 `#[serde(default)]` + `impl Default`。
- 置信度：高。

### L3. RuntimeConfigLease 非隔离恢复分支只对旧版租约生效
- 位置：backend/src/codex_config.rs 1222-1255、1745-1791、1852-1881；lease 字段 106-116；常量 68-69。
- 类型：过时兼容。
- 描述：当前所有写 lease 的路径固定 `isolated_runtime_constraints: true, independent_prompt_sources: true`（544-559），`restore_runtime_subagent_files` 首行对本版本 lease 恒早退；tests.rs 中无该分支的构造用例。
- 建议：确认上一个仍会写出非隔离 lease 的发布版本后删除约 130 行；或保留一个版本窗口并加 `// remove after vX` 注释。
- 需确认：v0.10.3 之前多少个版本会写出 `isolated_runtime_constraints=false`。
- 置信度：中。

### L4. 启动路径对同一 config.toml 文本重复 toml_edit 解析 3-4 次
- 位置：backend/src/codex_config.rs 134-137（`read_codex_config` 返回 `snapshot.raw()` 丢弃已解析文档）、373-399、1979、1902-1925。
- 类型：性能（低收益）/ 结构。
- 建议：`read_codex_config` 返回 `Arc<ConfigSnapshot>`，下游用 `snapshot.document().clone()`；`patch_config_with_fastctx_mode` 接收并返回 `DocumentMut`。
- 收益：config.toml 通常小于 10 KB，节省约 1-3 ms；主要收益是消除"字符串 → 文档 → 字符串 → 文档"的往返。
- 置信度：高（事实）/ 低（收益）。

### L5. local_router 三套 SSE 收集与解析函数同构
- 位置：backend/src/local_router.rs 9996-10062（Anthropic `collect_*` / `parse_*_bytes`）、10755-11016（Chat 同名两函数）、10063-10221 与 10804-10985（`stream_*_as_responses` / `emit_*_stream_event`）。
- 类型：重复实现。
- 描述：两组 collect/parse 函数除累积器类型与 `[DONE]` 判定外逐行一致（compact_sse_buffer → extend → ensure_sse_buffer_within_limit → take_next_sse_frame → sse_frame_data → ingest → 尾帧处理）。
- 建议：
  ```rust
  trait SseAccumulator {
      fn ingest(&mut self, data: &str) -> Result<()>;
      fn finished(&self) -> bool;
  }
  async fn collect_sse<A: SseAccumulator>(prepared: &mut PreparedUpstreamResponse, acc: &mut A, probe: Option<&RouteRequestLogProbe>, label: &str) -> Result<()>;
  fn parse_sse_bytes<A: SseAccumulator>(bytes: &[u8], acc: &mut A) -> Result<()>;
  ```
  Chat 的 `[DONE]` 在 `ingest` 内识别并置 `finished`。
- 置信度：高。

### L6. codex_config_guidance.rs 历史版本常量线性增长
- 位置：backend/src/codex_config_guidance.rs 491-646、858-871、1195-1223；消费者 1251、1357、1369。
- 类型：可维护性。
- 描述：子代理提示 15 版、FastCtx 提示 12 版、协作提示 11 版全文常量保留在源码中（约 900 行），每次改提示词需新增常量并插入列表，V8/V9/V10 已乱序；每次渲染对文本做 O(版本数 × 文本长度) 子串扫描。
- 建议：注入文本用带版本号的分隔标记包裹（如 `<!-- codey:subagent-guidance v15 -->…<!-- /codey -->`），清理时按标记删除；旧无标记版本只保留一个迁移期。
- 置信度：中。

### L7. 基准结果 JSON 被跟踪且无引用
- 位置：backend/benches/local_router_results.json、local_router_stability_results.json、local_router_tail_results.json（合计 257 KB、9.6K 行）。
- 类型：无用资源。
- 描述：全仓库无任何代码、脚本、CI、文档引用这三个文件。
- 建议：移出仓库（放 Release 附件或 docs/perf/），或在 INTERNAL_DEVELOPMENT.md 中声明其用途并加 `.gitattributes` 标记为生成文件。
- 置信度：高。

### L8. 请求日志查询每次探测 6 次列存在性，搜索为多列 LIKE 全表扫描
- 位置：backend/src/route_request_log.rs 1891-1995（`query_sqlite_route_request_logs`）、2009-2064（`sqlite_query_filters`）、1700-1760（索引：time、provider_model、status）。
- 类型：性能。
- 描述：每次分页查询打开只读连接后执行 6 次 `PRAGMA table_info` 判断迁移列是否存在，然后 `SELECT COUNT(*)` + 分页 SELECT；搜索条件对 8-10 列做 `LIKE '%x%'`，无法使用索引。schema 已到 v6，写入侧 `ensure_sqlite_log_columns` 已保证列存在。
- 影响：日志达数十万行时搜索与翻页为全表扫描，每页两次；前端 300 ms 防抖后每次输入都触发。
- 建议：读侧固定列集（打开时迁移一次即可，去掉 `has_*` 分支与 6 次 PRAGMA）；搜索限定为 request_id / trace_id / upstream_request_id 前缀匹配（可用索引）加可选 FTS5 表；或至少把 COUNT 结果按筛选条件缓存到 `page` 变化不重算。
- 验证：`EXPLAIN QUERY PLAN` 对比；在 20 万行测试库上计时 `query_route_request_logs`。
- 置信度：高（事实）/ 中（用户可感知程度）。

### L9. vendor 与 backend 之间 8 类工具函数重复且部分语义分叉
- 位置：backend model_list.rs 11-49 vs vendor model_catalog.rs 569-595（`/models` 端点推导：backend 双候选并去 `/responses` 后缀，vendor 单候选）；model_list.rs 51-103 vs vendor 597-631（模型 JSON 解析键集不同：backend 含 `slug`）；model_id.rs 28-43 vs vendor 734-745；sqlite_util.rs 7-17 vs vendor provider_sync.rs 973-981（逐字节相同，sqlite_util 注释已说"此前散落 6 处"）；`ensure_root_table` 3 份；`read_optional/remove_optional/restore_optional` 3 份；`timestamp_millis` 2 份。
- 类型：跨 crate 重复。
- 描述：当前实际使用的是 backend 侧实现；vendor 侧副本随 H4 删除即可消除大部分。
- 建议：随 H4 处理；保留的 `table_columns`、`read_optional`、原子写下沉到 codey-runtime-core 供两侧共用。
- 置信度：高。

### L10. 前端生产入口文件承载 880 行开发用 Mock
- 位置：src/main.tsx 30-912（`if (import.meta.env.DEV)` 块内的 previewConfig、mock invoke API）。
- 类型：结构混乱。
- 描述：整个文件 923 行，实际启动逻辑只有 913-923 的 `createRoot(...).render(...)`。DEV 守卫下生产包会被 tree-shake，无体积影响。
- 建议：移到 `src/dev/mockApi.ts`，在 main.tsx 中 `if (import.meta.env.DEV) await import("./dev/mockApi")`。
- 置信度：高（低风险）。

### L11. 前端 prop drilling：ModelSection 52 个 props，App.tsx 30 余个 useStableEvent 透传
- 位置：src/App.tsx 932-1010（handler 声明）、1258-1360（子组件挂载）；src/ModelSection.tsx props 类型（52 个字段）；OperationsPanel 13 个。
- 类型：耦合 / 职责不清。
- 描述：App.tsx 自身只有 8 个 useState，状态实际在 useModelSelection、useRuntimeStatus、useAppUpdates 等 hook 中，但所有操作再由 App 用 useStableEvent 包一层后逐个传给 ModelSection。
- 建议：ModelSection 直接消费 `useModelSelection` 返回对象（或通过 context 提供），App 只保留布局与全局 notice/confirm。
- 置信度：高（结构事实）。

### L12. 注入脚本各自挂 document 级 subtree 观察器，强制扫描遍历全 DOM
- 位置：public/codey-bridge.js 243（共享 `__codeyMutationDispatcher`）、public/model-whitelist-inject.js 1337-1341 与 1350-1359（`groupedMenuObserver` 观察 `document.body` subtree；`reactModelStateNodes(forceScan)` 取 `querySelectorAll("*")` 前 600 节点，随后 `scanReactObjectGraph` 深搜最多 3 万对象、深度 12）、public/prompt-optimize.js 722-741、public/codey-inject.js 3841、public/renderer-inject.js 1174；codey-inject.js 3877-3883 两个常驻 setInterval（15 s 与 60 s）。
- 类型：性能嫌疑。
- 描述：codey-bridge 已提供共享分发器，其余脚本有订阅分支但在 `snapshot().observerInstalled` 为假时各自回退到独立 observer，实际是否命中共享路径取决于加载顺序。
- 建议：先实测再定级：在 `handleGroupedMenuMutations`、`handleSessionToolMutations`、`handleComposerMutations` 入口加计数与耗时上报（Performance.mark），确认菜单打开与流式输出期间的回调频率；`forceScan` 只在明确的用户操作（打开模型菜单）后触发一次并记录耗时。
- 置信度：中（源码只能定嫌疑，参见性能审计约束）。

### L13. 三套备份目录/保留策略实现
- 位置：backend/src/config.rs 1988-2024（三份滚动备份）；codex_config.rs 523-537 与 3251-3295（lease backup_dir，保留 5 个，即使无内容也创建空目录）；session_index_cleanup.rs 562-620（`create_backup`、`unique_backup_dir`、`prune_backups`）。
- 类型：重复实现。
- 建议：抽 `fs_util::BackupSet { root, keep }` 提供 `create_unique_dir`、`prune`；codex_config 仅在有 hooks 备份内容时创建目录。
- 置信度：高。

### L14. Hook 入口样板在两个二进制分支重复
- 位置：backend/src/subagent_gate.rs 133-224 与 backend/src/fastctx_route_gate.rs 56-90（stdin 限长读取、JSON 解析、stdout 写出、退出码）。
- 类型：重复实现。
- 描述：subagent_gate 已有 `HookMode::WithFastctx` 组合模式调用 fastctx 逻辑。
- 建议：抽 `hook_io::{read_input, write_output}`；确认 hooks.json 是否仍注册独立的 fastctx hook 命令，若否则 fastctx_route_gate 独立入口可删。
- 需确认：`build_runtime_hooks_file` 当前生成的 hooks.json 中是否包含独立 fastctx 命令。
- 置信度：高（重复事实）/ 中（可删性）。

---

## 三、一般优化

### G1. clippy 附加 lint 统计
- 55 处 `needless_pass_by_value`、33 处 `redundant_clone`，集中在 subagent_gate.rs（19）、commands/models.rs（8）、route_request_log.rs（7）、subagent_orchestrator.rs（4）、local_router.rs（4）。均为零风险机械修改；建议在 CI 中把这两个 lint 提升为 warn。
- 置信度：高。

### G2. local_router 运行路径 40 余处 `.expect("… poisoned")`
- 位置：backend/src/local_router.rs 398-428、1080-1246、1484-1507、1893-1949、2842-2856、6611、7275-7489。
- 描述：Cargo.toml 明确 `panic = "unwind"`，任何持锁期间的 panic 会毒化 std Mutex/RwLock，之后每个请求在 `expect` 处再次 panic，形成连锁。route_request_log.rs 2347 已有 `lock_unpoisoned` 辅助函数。
- 建议：统一改用 `lock_unpoisoned`（`into_inner` 恢复），或将 `snapshot: RwLock<Arc<RouterSnapshot>>` 换成已在依赖中的 `arc_swap::ArcSwap`（route_request_log 已使用 ArcSwapOption），读路径无锁。
- 置信度：中。

### G3. provider_models::fetch 循环内重复计算不变量
- 位置：backend/src/provider_models.rs 58-78。4 个仅依赖 profile 的布尔在每个 endpoint 迭代中重算。提到循环外即可。置信度：高。

### G4. model_catalog 写后回读校验双解析
- 位置：backend/src/model_catalog.rs 336-350、524-559、995-1012。`write_catalog` 后重新读文件解析仅为比对 slug 顺序，`prepare_cached_catalog_for_native_web_search` 两次全量回读。建议比对内存中刚序列化的字节。置信度：中。

### G5. 前端未被引用的导出
- src/uiClasses.ts：`inputShellClass`、`insetInputClass`、`compactSelectInputClass`、`flushCardClass`、`surfaceCardPaddingClass`；src/runtimeStatusPresentation.ts：`OptimizationFeatureIcon`、`InjectionStatusSummary`；src/routeShortNames.ts：`OFFICIAL_ROUTE_SHORT_NAME`、`MAX_ROUTE_SHORT_NAME_CHARACTERS`、`routeShortNameCharacterCount`；另有 20 余个仅类型导出（无运行时影响）。删除前跑 `pnpm run test:js`，因 tests/mantine-wrapper.test.mjs 等读取这些文件文本。置信度：高。

### G6. commands/models.rs 3512 行拆分
- 建议按现有函数群拆为 `models/{sync,selection,defaults,catalog_refresh,renderer_catalog}.rs`：88-335 provider 同步、838-1335 选择保存与校验、1335-1500 默认模型、1539-1730 native 模型状态、1911-2110 renderer 目录、2116-2280 目录刷新与回滚。置信度：高（结构）。

### G7. codex_provider::sync_provider_profile 用派生 PartialEq 比较含 `#[serde(skip)]` 字段
- 位置：backend/src/codex_provider.rs 270-273；config.rs 34-35。可能因运行时填充的 `model_request_headers` 差异误判 changed 并递增 settings_revision。置信度：低，需在实际路径验证。

### G8. codex_config backup_dir 每次启动创建空目录并扫描 prune
- 位置：backend/src/codex_config.rs 523-537、3251-3295。仅在有 hooks 备份内容时创建。置信度：高（收益低）。

---

## 四、需要进一步确认业务依赖的项
| 项 | 需确认内容 |
|---|---|
| H3 TOML hooks 链 | 是否有旧版 Codex 从 config.toml 读取 hooks |
| H4 vendor 未使用模块 | vendor/CodeyRuntime 是否供其他项目使用；assets/inject 三个脚本是否有外部消费者 |
| L3 legacy lease 分支 | 哪个版本起 lease 固定为隔离模式，用户升级跨度 |
| L14 fastctx 独立 hook 入口 | hooks.json 是否仍注册独立 fastctx 命令 |
| config.rs `ccSwitchProviderId` / `ccSwitchReadOnly` 别名 | 用户磁盘是否仍有 CC Switch 时代配置 |
| G7 changed 判定 | 实际调用路径是否传入带请求头的 profile |

---

## 五、分阶段优化清单

**阶段 0：重构前置（1 周）**
- 测试验证：为 runtimeStatusPollScheduler、modelIds、routeShortNames、useModelSelection、model_id.rs 别名解析补 import 级/单元级行为测试；把读源码文本的 JS 测试分类为"必要幂等标记"与"伪依赖"。
- 监控：在 subagent hook allow 路径按 1/50 采样记录 latency_ms；注入脚本 observer 回调计数与耗时上报；`ConfigStore::save` 与 `query_route_request_logs` 加耗时日志。

**阶段 1：低风险删除（1 周）**
- 建议删除：L2 死字段与恒空迁移、8 个默认值函数；L7 三个 bench 结果 JSON；G5 前端未引用导出；G8 空备份目录创建。
- 确认后删除：H3 TOML hooks 链及对应测试；L3 legacy lease 分支；H4 vendor 未使用模块与 1.08 万行测试；vendor assets/inject 副本；L14 fastctx 独立入口。

**阶段 2：合并与重构（2-3 周）**
- 建议重构：H2 备份改 rename 链并 best-effort；H8 三套原子写统一到 fs_util；H7 别名解析统一到 model_id.rs 并打断 config ↔ local_router 双向依赖；L1 iLink 共享模块；L5 SSE 泛型累积器；L13 BackupSet；L4 ConfigSnapshot 复用；L8 请求日志读侧固定列集并调整搜索策略；G2 锁毒化处理或 ArcSwap；G1 机械修复。
- 最后做：H1 local_router 目录化拆分、G6 commands/models 拆分、L11 前端 ModelSection 去 prop drilling、L10 mock 移出入口。

**阶段 3：测试验证**
- 每阶段后：`pnpm run check`、`pnpm run test:js`、`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`git diff --check`。
- 专项：H2 用 fs_usage/strace 对比保存一次的 fsync 次数；L8 用 EXPLAIN QUERY PLAN 与 20 万行样本库计时；H7 含 `/` 模型名回归；H6 `sql_is_read_only` 对抗用例；Windows 实机验证启动与 hook 路径（本机无法验证）。

**阶段 4：持续监控**
- hook latency_ms 分布（allow 采样）；请求日志查询 p95；本地路由锁毒化 panic 计数（错误日志 `panic` 记录）；配置保存耗时；注入脚本 observer 回调频率。
