# 本地路由 / 非本地路由复审报告（2026-09-06）

范围：`backend/src/local_router.rs` 请求热路径、`config.rs` 线路快照、`codex_startup_patch.js` 发送入口、启动器并行化现状；对照 cc-switch `src-tauri/src/proxy` 源码。
前提：本仓库 09-05 已完成三轮路由延迟/稳定性/收尾优化，09-06 另一轮代码审查已落地 H2/H3/H4/H7/H8/L1/L2/L5/L10/G2 等项（见 `output/code-review-2026-09-06.md` 第六节）。本报告只列这些之外仍成立的问题。

## 一、调用链

本地路由开启：
1. renderer/主进程 `thread/start|resume|fork` 在发送前被启动补丁同步改写为 `modelProvider=codey_router`，删除可覆盖供应商的 config 键（`codex_startup_patch.js` 1046-1060）。纯字符串/对象操作，无 I/O。
2. app-server → `127.0.0.1:port/v1/responses`（HTTP 或 WS）。`LocalRouter::start` 单 accept loop，每连接一个任务，64 并发上限 + 4 个拒绝名额（`local_router.rs` 213-370）。
3. `handle_connection`：peek 判断 WS → 读头（30 s）→ `/healthz`、日志页 → 鉴权 → 读体（内存配额信号量）→ 分发。
4. `proxy_responses`：zstd 解压（blocking worker）→ JSON 解析（≥256 KiB 走 blocking worker）→ `proxy_parsed_responses_inner`。
5. `proxy_parsed_responses_inner`：快照读锁一次、bindings 互斥一次（哈希查找 + 绑定刷新）→ 别名还原 → 协议桥选择 → 头构造（官方登录态走 TTL 缓存，未命中 blocking 读 auth.json）→ prompt-cache-key（SHA-256，只哈希线路/模型/身份，不哈希正文）→ 原生 Responses 保留原始字节仅改写 `model/client_metadata/previous_response_id` → 可选上游 WS → HTTP 发送（响应头超时 30 s，非流式 5 min）→ 首包嗅探（Content-Type 或最多 1 KiB 前缀）→ 逐 chunk 写回（单次 `write_all` 一个 chunked 帧，TCP_NODELAY）。
6. 取消：WS 下游已并发中止上游等待；HTTP 下游依赖写失败退出。

本地路由关闭：Codex 直连供应商，Codey 不在请求路径上。相关成本只在启动（配置快照、模型目录、会话维护已并行，`launcher.rs` 673/1158/1184）与恢复入口的模型兼容转换。请求级 TTFT 无可优化点。

## 二、问题清单

### P0
无。热路径未发现阻塞调用、重复请求或串行等待；09-05 已确认的取消传播、终态去重、分帧重复扫描已修复。

### P1
| # | 位置 | 问题 | 影响 | 建议 | 风险 | 验证 |
|---|---|---|---|---|---|---|
| 1 | `local_router.rs` `write_anthropic_messages_as_responses` 9390-9430 | Anthropic 非 2xx 固定映射 502 JSON；Chat/原生路径透传状态码并写文本体 | Codex 对 5xx 重试数次，上游 401/400/429 需多轮失败后才显示，且 JSON 体被 Codex 显示为 Unknown error | **已实施**：非 2xx 先于协议桥分流，三种协议共用 `write_upstream_http_error` | Chat/原生路径此前已透传 401，Anthropic 与之对齐；用户可见错误从 502 变为上游实际状态与摘要 | 新增 `anthropic_upstream_http_error_keeps_status_and_safe_text`；真实线路观察 Codex 不再对 4xx 重试 |
| 2 | `reqwest::Client` 构建 213-270 | HTTP/2 池化连接无应用层 keepalive，NAT/负载均衡静默断连时下一请求先在死连接上失败 | 空闲后首个请求 TTFT 尾部抖动（假设，未测） | **已实施**：h2 PING 30 s/超时 10 s/空闲也发 | PING 流量可忽略；HTTP/1.1 无影响 | 真实供应商下对比空闲 5 min 后首请求 TTFT P95 |

### P2
| # | 位置 | 问题 | 处理 |
|---|---|---|---|
| 3 | `reason_phrase` 11872 | 固定表外状态码写 `429 OK` | **已实施**：改用 `http` 标准原因短语，未知写 `Unknown` |
| 4 | accept loop 292 | 每连接克隆 `RouterServer`（两份 token 字符串 + PathBuf） | **已实施**：`Arc<RouterServer>` |
| 5 | `read_http_request_body_with_budget` 6051 | 请求体分片读取多次倍增扩容 | **已实施**：拿到配额后按 `content-length` 一次 `reserve_exact` |
| 6 | 下游 `connection: close` | 每请求一条回环 TCP | 保留。回环建连远低于推理延迟，改造涉及请求边界与生命周期 |
| 7 | 上游 WS 连接超时 3 s → 回退 HTTP → 60 s 退避 | 慢网首轮最多多付 3 s | 既有取舍；系统代理生效的线路已在配置层禁用 WS |
| 8 | `normalize_responses_tool_parameter_roots` 4929 | 第三方线路工具 schema 根非 object 时丢弃原始字节整体重序列化 | Codex 内置工具不触发；如 MCP 工具普遍触发，把 `tools` 加入原始字节改写白名单 |
| 9 | 单文件 18.7k 行、错误日志每次独立 `spawn_blocking` | 可维护性 / 错误风暴 | 与前几轮结论一致，独立迭代 |

## 三、cc-switch 对照

可借鉴：错误分类决定可重试性、失败「中性释放」不污染健康度、熔断 HalfOpen 单探测名额、2xx 先取首包/校验首个语义事件再提交流。前者三项属跨线路容灾，Codey 明确不做；最后一项与现有嗅探一致。

不宜照搬：Anthropic 直连每请求新建 TCP+TLS（为保头大小写放弃连接池）；请求体无上限整包缓冲并 JSON 全量往返；401/403/429 也 failover 并异步改写当前供应商；无同线路重试与退避；每请求多次同步读 SQLite。其 reqwest 客户端未开 HTTP/2、TCP_NODELAY、空闲回收，无 SSE 心跳，不构成 TTFT 经验。

## 四、修改摘要

仅改 `backend/src/local_router.rs`（5 处生产代码 + 4 项测试）与 `INTERNAL_DEVELOPMENT.md`。未改选路、认证、请求字段、流式事件、超时、重试、并发上限或日志结构；唯一用户可见变化是 Anthropic 线路上游错误改为透传实际状态码。

验证：`cargo test -p codey --lib local_router` 181 通过（3 基准忽略）；`route_request_log::tests` 31 通过；`cargo clippy -p codey --lib --tests -- -D warnings`、`cargo fmt --check`、`git diff --check` 通过。JS 测试不读取该文件，未受影响。

## 四之二、2026-09-07 精简（另一会话已将 local_router.rs 拆为目录模块，以下改动位于新布局）

- `adapt.rs`：Chat 与 Anthropic 成功响应写回合并为 `write_adapted_upstream_as_responses`，删除约 90 行逐行重复；错误文案与 502 转换错误契约不变。
- `downstream.rs` / `websocket.rs`：`ResponsesDownstream` 只保留带探针的两个方法为必需/默认实现，不带探针版本转调；HTTP、WebSocket、观测包装各删一份重复方法。
- `upstream_response.rs`：透传状态行改用共享 `reason_phrase`。
- 验证：181 项路由测试、clippy -D warnings、fmt、diff --check 通过。

## 五、性能对比方案

基线：本轮修改前二进制；两侧相同 release 参数，复用 `local_router_bench.rs` 命令（见 INTERNAL_DEVELOPMENT.md「本地路由延迟与协议审查」）。指标：
- 会话建立：`thread/start` 发出到首个 app-server 响应（renderer trace）。
- TTFT：请求日志 `upstream_first_byte_ms`；首次响应：`downstream_first_content_ms`。
- 持续输出：每秒 `output_text.delta` 字节数；总耗时：`total_ms`。
- 并发：1/8/32/64 并发成功请求/秒、P50/P95。
- 错误率/超时率：请求日志 status≠2xx 与 504 占比；资源：进程 CPU ms/512 请求、峰值 RSS。
本地 mock 每次关闭上游连接，测不到 h2 keepalive 收益；需在真实供应商上做「空闲 5 min 后首请求」专项对比，才能把第 2 项从假设变为结论。
