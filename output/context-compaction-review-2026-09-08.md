# Codey 同步线路上下文管理与远程压缩审查（2026-09-08）

> 范围：Codex Desktop → Codey 本地路由（回环 HTTP/SSE/WS）→ 上游供应商这条“同步转发线路”上，与上下文窗口、Token 统计、自动压缩（compaction）相关的全部代码路径。
> 依据：本仓库 `backend/src/**`、`vendor/CodeyRuntime/**`、本机 `~/.codex/model-catalogs/*.json`，以及 2026-09-08 从 GitHub `openai/codex` main 分支直接拉取的源码（`codex-rs/core/src/compact.rs`、`compact_remote.rs`、`compact_remote_v2.rs`、`compact_model_fallback.rs`、`session/turn.rs`、`session/context_window.rs`、`models-manager/src/model_info.rs`、`protocol/src/openai_models.rs`、`model-provider/src/provider.rs`、`features/src/lib.rs`、`codex-api/src/sse/responses.rs`）。
> 结论性质：源码层面的事实与嫌疑。凡未实机复现的行为，均在文中标注“待验证”。

---

## 0. 术语与假设

| 术语 | 本文含义 |
| --- | --- |
| 同步线路 | Codex 发起一次 `/responses` 或 `/responses/compact` 请求，Codey 在同一连接内完成选路、转换、转发和回写的路径。**假设**：用户所说的“同步线路”指这条请求转发线路，而不是 `commands/models/sync.rs` 里的“线路模型同步”功能。若指后者，请指出，本报告第 1～4 节仍然成立，但第 6 节的改造项需要重排。 |
| 远程压缩 | Codex 的 remote compaction：把整段历史交给服务端返回不透明 `{"type":"compaction","encrypted_content":…}` 项，替换本地历史。V2 通过在 `/responses` 输入末尾附加 `{"type":"compaction_trigger"}` 触发；旧版通过 `POST /responses/compact`。 |
| 本地压缩 | Codex 用 `SUMMARIZATION_PROMPT` 发起一次普通 `/responses` 请求，把模型回复当摘要，重建历史。 |
| 原生线路 | `upstream_protocol = openai_responses` 或官方账号线路；Codey 直接透传 Responses。 |
| 适配线路 | Chat Completions / Anthropic Messages 线路；Codey 做协议转换。 |

---

## 1. 当前实现的架构与问题总结

### 1.1 职责边界（谁在做什么）

```
┌────────────── Codex core（不在本仓库，行为由上游决定） ──────────────┐
│ 持有会话历史、Token 记账、触发压缩、执行压缩、回写 rollout           │
│ 触发条件：context_window_token_status() → token_limit_reached          │
│ 能力判定：provider.name == "openai" → RemoteCompactionSupport::V2      │
│ 模型切换：maybe_run_previous_model_inline_compact()                    │
└────────────────────────────┬───────────────────────────────────────────┘
                             │ 读取 model_catalog_json / config.toml / usage
┌────────────── Codey（本仓库） ─────────────────────────────────────────┐
│ ① 写模型目录：context_window / max_context_window /                    │
│    effective_context_window_percent / auto_compact_token_limit(null)   │
│    backend/src/model_catalog.rs:16-17, 1252-1270                       │
│    vendor/.../model_suffix.rs:283 (sanitize → auto_compact 置 null)    │
│ ② 决定 provider 名：全部运行时线路都原生支持 → "openai"，否则          │
│    "codey_router"                                                       │
│    backend/src/config.rs:1097-1136, codex_config.rs:2546-2555          │
│ ③ 转发：/responses(含 compaction_trigger) 与 /responses/compact         │
│    backend/src/local_router/responses.rs:212-222, 1078-1081            │
│    backend/src/local_router/upstream.rs:202-205, 281-293               │
│ ④ 适配线路丢弃不透明项（compaction/reasoning/encrypted_content），      │
│    compaction_trigger 无法转换 → 400                                    │
│    backend/src/local_router/chat_tools.rs:62-69, 173-179               │
│ ⑤ 转发 usage（不自行计数）                                             │
│    sse_chat.rs:196-225, sse_anthropic.rs:162-177, chat_request.rs:87   │
│ ⑥ 请求日志按 request_kind 记 "responses"/"responses_compact"           │
│    server.rs:398-410, route_request_log.rs:143-210                     │
└────────────────────────────────────────────────────────────────────────┘
```

**核心结论：Codey 不拥有上下文，也不执行压缩。** 它通过两个旁路间接影响 Codex 的行为：模型目录里的窗口字段，和 provider 名称。所有“压缩什么时候发生、保留什么、失败怎么办”的逻辑都在 Codex core 内。

### 1.2 Codex 侧关键事实（上游源码核实）

| 事实 | 位置（openai/codex main） |
| --- | --- |
| `auto_compact_token_limit()` = `min(配置值, resolved_context_window × 9 / 10)`；配置为空时就是 90 % | `protocol/src/openai_models.rs:515-525` |
| `usable_context_window()` = `context_window × effective_context_window_percent / 100`（目录默认 95） | 同上 `:509-513` |
| `token_limit_reached` = 按 scope 统计的用量 ≥ auto_compact_limit(+fallback buffer) **或** 活跃用量 ≥ usable window | `core/src/session/context_window.rs:57-125` |
| 活跃用量来自上游 `usage`（`get_total_token_usage`），没有 usage 时用本地字节估算 | `core/src/context_manager/history.rs:439-465, 677` |
| 采样后检查：`needs_follow_up && token_limit_reached` → 立即在轮次内压缩（同步阻塞） | `core/src/session/turn.rs:463-530` |
| 轮次前检查：`token_limit_reached` → PreTurn 压缩 | `turn.rs:1088-1113` |
| 远程压缩能力：`is_openai()`（name == "openai"）或 Azure Responses → V2；否则 Unsupported | `model-provider/src/provider.rs:353-364` |
| `remote_compaction_v2` 特性默认开启 → 使用 `compaction_trigger`；关闭时才走 `/responses/compact` | `features/src/lib.rs:1732-1737`, `turn.rs:1277-1330` |
| 本地压缩：ContextWindowExceeded 时删最旧一项重试；其他错误按 `stream_max_retries`（默认 4）退避重试 | `core/src/compact.rs:299-352` |
| 本地压缩保留：最近用户消息 ≤ 20 000 token + 摘要（`SUMMARY_PREFIX` + 助手回复） | `compact.rs:63, 683-760` |
| 远程 V2 保留：user/assistant 消息 ≤ 64 000 token，丢弃 function call/output、reasoning、developer 消息 | `compact_remote_v2.rs:76-80`, `compact_remote.rs:374-401` |
| 超窗兜底：把最旧工具输出改写成占位文本直到估算 ≤ window | `compact_remote.rs:403-528` |
| 模型切换：`comp_hash` 变化 → 用**上一个模型**先压缩（CompHashChanged）；切到更小窗口且用量超新限 → ModelDownshift；失败后仅在 Codex 后端登录且 is_openai 时用当前模型重试 | `turn.rs:1130-1245`, `compact_model_fallback.rs:9-20` |
| 上下文超限的识别只看 SSE `response.failed` 里 `error.code == "context_length_exceeded"` | `codex-api/src/sse/responses.rs:423-424, 711-712` |
| config.toml 支持 `model_context_window`（被 `max_context_window` 夹住）、`model_auto_compact_token_limit`、`model_auto_compact_token_limit_scope`（total / body_after_prefix）、`compact_prompt`、`experimental_compact_prompt_file` | `models-manager/src/model_info.rs:25-37`；developers.openai.com/codex/config-reference |
| 压缩结果写入 rollout：`compacted` 项含 `replacement_history` 与 `compaction_model_hash` | `core/src/session/mod.rs:3771-3846` |

### 1.3 问题总览（详细清单见第 6 节）

1. **第三方模型的上下文窗口不是真实值**：合成模型继承官方模板的 `context_window=272000 / max_context_window=872000 / percent=95`。本机目录中 `route-…/glm-5.3-flash`、`deepseek-v4-flash`、`qwen3.8-flash` 均为 272000/872000/95。窗口更小的模型会在压缩触发前被上游拒绝；窗口更大的模型被过早压缩。
2. **除“1M”开关外没有任何上下文配置项**；`auto_compact_token_limit` 被无条件置 null。
3. **上下文超限错误语义在 Codey 层丢失**：Codey 把上游 4xx 改写为纯文本 `upstream_http_error`，自造的 `response.failed` 也只带 Codey 错误码，Codex 无法识别 `context_length_exceeded`，本地压缩的“删最旧一项重试”兜底永不触发。
4. **provider 名是运行时全局的**：任一适配线路会让所有线路（包括官方账号）退回本地压缩；改变该状态需要重启，而热更新新增的适配线路在重启前会收到 `compaction_trigger` 并以 400 失败。
5. **跨线路的不透明 compaction 项无保护**：原生线路把 A 供应商产生的 `encrypted_content` 原样发给 B 供应商；适配线路把它静默丢弃，压缩后的历史随之消失。
6. **可观测性不足**：V2 压缩请求被记为普通 `responses`，无法从请求日志区分压缩与正常轮次；请求日志和错误日志均不含请求体（这是好的），但也没有压缩结果的长度/用量标记。
7. **第三方 Responses 线路无法在 UI 声明“支持远程压缩”**：`supports_remote_compaction` 只在从 Codex 配置导入且 provider 名为 `OpenAI` 时为 true，保存时始终沿用旧值（`commands.rs:1610-1637`）。

---

## 2. 当前是否支持配置上下文大小

**结论：不支持通用配置。** 现状只有三条路径：

| 路径 | 作用范围 | 位置 | 限制 |
| --- | --- | --- | --- |
| 逐模型“1M”开关（`supports1MContextByProvider`） | 单线路单模型 | `config.rs:562-563, 905-945`；`model_catalog.rs:1252-1270`；`ModelSection.tsx:900-913` | 只能在 272k（模板值）与 1 000 000 两档间切换；同时把 percent 改 100、auto_compact 置 null |
| 官方模板继承 | 所有第三方合成模型 | `model_catalog.rs:1184-1222`（`synthetic_model`）、`model_suffix.rs:283` | 窗口值来自 `models_cache.json` 中的官方模型（当前 272000/872000），与真实模型无关 |
| 用户手改 `~/.codex/config.toml` 的 `model_context_window` / `model_auto_compact_token_limit` | 全局，对所有模型生效 | Codex 侧 `model_info.rs:25-37` | Codey 从不写这些键（`codex_config.rs` 无匹配）；值会被 `max_context_window` 夹住；不能按线路/模型区分 |

`vendor/CodeyRuntime/.../model_suffix.rs` 中的 `deepseek-v4-pro[1M]` 后缀语法与 `build_model_catalog_json` 在 backend 中**没有调用方**（`grep build_model_catalog_json|suffix_window backend/src` 为空），属于遗留能力。

要支持配置，需要改动：

- 数据结构：`CodeyConfig` 新增 `model_context_by_provider: BTreeMap<provider_id, BTreeMap<model, ModelContextConfig>>`（见 3.1），替代或包含 `supports_1m_context_by_provider`（保留旧字段做迁移）。
- 目录生成：`model_catalog.rs::refresh_for_provider_with_capabilities` 与 `prepare_cached_catalog_for_current_capabilities` 增加 `context_configs` 参数；`configure_1m_context_window` 泛化为 `apply_context_config`。
- 热更新判定：`commands/models/state.rs` 中 `provider_route_snapshots` / `runtime_supports_current_routes_for_hot_reload` 把窗口配置纳入“目录字段可热更”而非“需重启”（目录文件由 Codex 在新线程启动时读取，需确认是否需要重启；见第 8 节问题 Q3）。
- 前端：`useModelSelection.ts` 的 `draft1MModels` 扩展为每模型的窗口输入；`App.types.ts` 与 `mockApi.ts` 同步。
- 命令：`save_selected_models` 参数新增 `modelContextConfigs`（该函数已允许 `too_many_arguments`）。
- 渲染目录：`commands/models/state.rs:150-200` 的 `context_window/max_context_window` 元数据改为读取配置值。
- 注入脚本：`public/model-whitelist-inject.js:650-651, 762-763` 已按元数据透传 `context_window`，无需改动。

---

## 3. 推荐的上下文配置模型

### 3.1 数据结构

```rust
// backend/src/config.rs
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelContextConfig {
    /// 真实窗口（token）。None = 沿用目录模板值。
    pub context_window: Option<u64>,
    /// 硬上限，默认 = context_window。Codex 用它夹住 model_context_window。
    pub max_context_window: Option<u64>,
    /// 可用比例，默认 95；1M 模型建议 100。
    pub effective_context_window_percent: Option<u8>,
    /// 触发自动压缩的 token 数；None = Codex 默认（90 %）。
    pub auto_compact_token_limit: Option<u64>,
    /// 压缩策略：Inherit（按 provider 名）| RemoteNative | CodeyLocal（见第 4 节）
    pub compaction_mode: CompactionMode,
}
```

存储键：`provider_id → upstream_model → ModelContextConfig`，沿用 `supports_1m_context_by_provider` 的 provider 作用域与 `retain_*` 清理规则（`config.rs:792, 939`）。

### 3.2 生效规则

1. 目录写入：`context_window = cfg.context_window ?? template`；`max_context_window = cfg.max ?? context_window`；`percent = cfg.percent ?? (1M ? 100 : 95)`；`auto_compact_token_limit = cfg.auto ?? null`。
2. 校验：`auto_compact_token_limit < context_window × percent / 100`，否则拒绝保存并提示“压缩阈值必须小于可用窗口”。预留空间建议默认 `context_window × 10 %`，与 Codex 的 90 % 规则一致；提供“预留 token 数”而非百分比的高级项时，转换为 `auto_compact_token_limit = usable − reserve`。
3. 默认值来源：允许从上游 `/models` 同步结果读取 `context_length`/`max_input_tokens` 等常见字段（OpenAI 兼容中转常返回），作为“建议值”展示，不自动写入（遵守“无法确认不猜测”的项目原则）。
4. 迁移：读取旧 `supports1MContextByProvider` 时生成 `context_window=1_000_000, percent=100` 的配置项，保留旧字段一个版本。
5. 不写 Codex 全局 `model_context_window`：它会覆盖所有模型且被 `max_context_window` 夹住，与逐模型目录冲突。

---

## 4. 自动压缩流程与状态机

### 4.1 现状的状态机（Codex 拥有，Codey 旁观）

```
Idle ──usage 更新──► Check(token_status)
  │                         │ token_limit_reached
  │                         ▼
  │                Compacting(inline, 阻塞当前轮)
  │                  ├─ provider=openai ─► RemoteV2: /responses + compaction_trigger
  │                  │        └─ 失败且可重试 & 有 fallback ctx ─► 用当前模型重试
  │                  ├─ provider≠openai ─► Local: /responses + SUMMARIZATION_PROMPT
  │                  │        ├─ ContextWindowExceeded ─► 删最旧一项 → 重试
  │                  │        └─ 其他错误 ─► 退避重试 ≤ stream_max_retries
  │                  ▼
  │            Installed(replace_compacted_history, 写 rollout `compacted`)
  │                  │
  └──────────────────┘ recompute_token_usage → 回到 Idle
```

Codey 在这条链上的可见点只有两次 HTTP 请求（压缩请求本身，以及压缩后的下一次请求）。

### 4.2 建议：Codey 的角色是“兜底与护栏”，不是第二个压缩引擎

原因：Codex 已有阈值、预留、防抖（`take_new_context_window_request`、单轮内串行）和 rollout 持久化；在代理层重复实现会与 Codex 的 Token 记账脱节，还会破坏 `previous_response_id`/prompt cache 前缀。Codey 应补齐 Codex 做不到的三件事：

**A. 让 Codex 的阈值正确**：第 3 节的逐模型窗口配置。这是所有其他方案的前提。

**B. 让 Codex 的失败路径能工作**：把上游 `context_length_exceeded` 语义透传（第 6 节 M1）。

**C. 为适配线路提供 Codey 本地摘要（CodeyLocal 模式）**，仅在以下场景启用：

- 运行时 provider 名为 `openai`（存在需要远程压缩的原生线路），但某条线路是适配线路或不支持 compaction，且请求携带 `compaction_trigger`；
- 或用户为某模型显式选择 `compaction_mode = CodeyLocal`。

流程（在 `proxy_parsed_responses_inner` 中、协议转换之前插入）：

```
收到 /responses，input 末项为 compaction_trigger，且 bridge ≠ NativeResponses 或线路不支持远程压缩
  1. 去掉 compaction_trigger；把 Codey 自有 compaction 项（见 4.3）解码为文本消息
  2. 构造摘要请求：instructions = Codex SUMMARIZATION_PROMPT 同义文本（Codey 自带，可配置）
     input = 原历史（已转换）；stream=false；max_output_tokens = min(8k, usable/8)
  3. 上游返回文本 S
     - 校验：非空、approx_tokens(S) ≤ 摘要预算；否则 → 步骤 5
  4. 合成 Responses 流：response.output_item.added{type:"compaction", id:"cmp_codey_…",
     encrypted_content: base64(json{v:1, provider_id, model, summary:S, created_at})}
     + response.completed；usage 取上游 usage
  5. 失败（超时/上游错误/校验失败）：返回 response.failed{code:"codey_compaction_failed"}，
     不改写任何历史（Codex 侧保持原历史并报错，用户可 /new）
```

时机分析：

| 方案 | 优点 | 缺点 | 结论 |
| --- | --- | --- | --- |
| 请求发送前（拦截 compaction_trigger，同步完成） | 与 Codex 状态机一致；结果由 Codex 安装并写 rollout；无并发问题 | 阻塞当前轮（Codex 本身也如此）；需要一次额外上游调用 | **推荐** |
| 对话过程中（Codey 在普通请求里悄悄裁剪 input） | 无需 Codex 配合 | Codex 记账与实际 input 脱节；破坏 prompt cache 前缀；工具调用链被截断的风险；无法持久化 | 不采用 |
| 后台异步（Codey 预先生成摘要缓存，下一次触发时直接返回） | 触发时零延迟 | 摘要与最新历史不一致；需要 Codey 持有会话历史副本（内存与隐私成本）；与 Codex 的 window_number 语义冲突 | 不采用；可作“可选增强”里的预热策略研究 |

### 4.3 Codey 自有 compaction 载体

`encrypted_content` 对 Codex 是不透明字节串，Codex 会在后续请求中原样回传（`should_keep_compacted_history_item` 保留 `Compaction` 项）。Codey 用带前缀 `codey1:` 的 base64 JSON 承载明文摘要：

- 发往适配线路：解码成一条 `role:user` 文本 `【上下文摘要】…`（Anthropic 线路放入首条 user 消息或 system 之后，保持 user/assistant 交替）。
- 发往原生线路：同样解码为文本消息，**不能**把伪造的 encrypted_content 发给 OpenAI。
- 非 Codey 前缀的 compaction 项：来自真实服务端，只能透传给产生它的线路（见第 5 节）。

这样压缩结果对所有线路可移植，解决第 5 节“压缩模型与主模型不一致”的大部分场景。

### 4.4 顺序、角色与工具链的一致性保证

- 只在 `input` 末项为 `compaction_trigger` 时介入；其他请求不改写历史顺序。
- 解码后的摘要消息插入位置 = 原 compaction 项位置，保持相对顺序。
- 适配转换已保证 `function_call` 与 `function_call_output` 成对（`chat_tools.rs` 现有逻辑）；Codey 摘要请求不带 tools 字段，避免上游产生工具调用。
- 会话一致性由 Codex 的 `replace_compacted_history` 保证；Codey 不写 rollout、不改 SQLite。

### 4.5 降级策略矩阵

| 情况 | 处理 |
| --- | --- |
| 上游不可达/超时 | `response.failed{code:"codey_compaction_failed"}`；Codex 保持原历史并显示错误；不重试（遵守“不重放可能已送达的请求”） |
| 摘要为空/过长 | 同上，并在错误日志记录 `summaryTokens`、`budget` |
| 压缩后仍超限 | Codex 下一轮仍 `token_limit_reached` 会再次触发；Codey 对同一 thread 在 60 s 内的第二次 CodeyLocal 压缩返回 `codey_compaction_loop`，提示新建线程（防抖） |
| 消息异常（无可转换项） | 沿用现有 400 `unsupported_responses_payload` |
| 远程服务不可用但线路是原生 | 不介入，透传上游错误（附 M1 的错误码透传） |

---

## 5. 模型切换兼容方案

### 5.1 现状

- 切换流程：渲染层选择带线路前缀的别名 → Codex 以该别名发请求 → Codey `target_for_request` 解析线路/模型，并按 `thread-id/session-id` 记住绑定（`responses.rs:869-895`，`server.rs:412-470`）。压缩请求带同样的头，因此落到同一线路。
- Codex 在切换后的第一轮：`comp_hash` 变化 → 用**旧模型**压缩；新窗口更小且超限 → 用旧模型压缩（ModelDownshift）。这两次压缩请求的 `model` 是旧模型别名，Codey 会路由到旧线路。
- Codex 用当前模型兜底重试的条件是“Codex 后端登录 + provider 是 openai”。Codey 官方线路满足，API-key 线路不满足。

### 5.2 场景处理

| 场景 | Codex 行为 | Codey 现状风险 | 设计 |
| --- | --- | --- | --- |
| 主模型切到更小窗口 | ModelDownshift：先用旧模型把历史压到新限之下 | 旧线路已删除/禁用 → 旧模型别名解析失败（404 `model_not_enabled`）→ 轮次失败；第三方目录窗口不真实使新限判断失真 | 第 3 节真实窗口；旧线路缺失时 Codey 返回 `route_missing_for_compaction` 明确提示；CodeyLocal 载体让摘要在新线路可读 |
| 主模型切到更大窗口 | 不压缩，历史继续累加 | 无 | 无需处理；文档说明“不会重复压缩” |
| 压缩模型 ≠ 主模型（跨线路） | OpenAI 后端产生的 `compaction` 项 → 下一轮发给新线路 | 原生第三方 Responses 线路：上游多半 400；适配线路：静默丢弃，历史丢失 | 记录 `cmp_id → provider_id`（内存 LRU，随 RouteBindings 一起）；发往其他线路时返回 409 `compaction_not_portable`，提示“该会话已由 X 线路压缩，请切回或新建线程”；Codey 自有载体则解码可用 |
| 压缩过程中发生模型切换 | Codex 单轮内串行，切换在下一轮生效 | Codey 无并发压缩问题；但热更新可能在压缩请求发出前替换 snapshot | 压缩请求解析线路时使用请求开始时的 snapshot（现已如此，`Arc::clone(snapshot)`）；不做额外处理 |
| 压缩完成后再次超过新模型限制 | 下一轮 `token_limit_reached` 再次压缩 | 若 CodeyLocal 摘要本身超预算会循环 | 4.5 的防抖与预算校验 |

### 5.3 Token 估算与格式差异

- Codex 记账用上游 `usage.total_tokens`；不同供应商 tokenizer 不同，切换后首轮的记账来自旧供应商。Codey 不应尝试重算，只需确保 usage 正确映射（现状 Chat/Anthropic 映射完整，含 cached 与 reasoning）。
- 当上游不返回 usage（部分中转不支持 `stream_options.include_usage`），Codex 退回字节估算（约 4 字节/token）。建议在请求日志 `usage_unavailable_reason` 出现时，在控制台线路卡上提示“该线路不返回用量，压缩阈值按估算触发”。
- 提示词格式：CodeyLocal 摘要请求只用纯文本 instructions，不依赖 reasoning/encrypted 项。
- 工具调用兼容：跨线路切换后 `function_call` 的 `call_id` 格式可能不被新供应商接受（现有 `chat_tools.rs` 已做名称规范化）；压缩后历史不含工具项（Codex V2 与 CodeyLocal 都丢弃），切换后风险最低的时机正是“刚压缩后”。

---

## 6. 远程压缩逻辑问题清单

编号前缀：M=必须修复，S=建议优化，O=可选增强。位置均为本仓库文件。

| # | 问题 | 证据 | 影响 | 级别 |
| --- | --- | --- | --- | --- |
| M1 | 上下文超限错误语义丢失 | `errors.rs:122-193` 把任何上游 4xx 写成纯文本 `upstream_http_error`；`sse_responses.rs:496-527` 的 `response.failed` 只带 Codey 码。Codex 仅识别 `error.code=="context_length_exceeded"`（上游 `codex-api/src/sse/responses.rs:711`） | 本地压缩的删项重试永不触发，改为 4 次盲重试后报错；主轮次超限也无法被 Codex 正确提示 | 必须 |
| M2 | 第三方模型窗口继承模板值 | `model_catalog.rs:1184-1222` 未改 `context_window/max_context_window`；本机目录 `route-…/glm-5.3-flash 272000/872000/95` | 小窗口模型在压缩前被拒；大窗口模型过早压缩 | 必须 |
| M3 | 热更新窗口期的 `compaction_trigger` 落到适配线路 | `state.rs:23-28` 仅标记需重启；`chat_tools.rs:62-69` 转换时 bail | 新增 Chat 线路后到重启前，任何触发压缩的轮次直接 400 | 必须 |
| M4 | 跨线路不透明 compaction 项无保护 | `responses.rs` 无对 `compaction` 项的检查；`chat_tools.rs:173-179` 静默丢弃 | 切线路后历史静默丢失或上游 400 | 必须 |
| S1 | provider 名全运行时统一，任一适配线路关闭官方远程压缩 | `config.rs:1112-1136` | 官方账号退回本地压缩，质量下降、耗时增加 | 建议（4.2C 落地后可保持 `openai`） |
| S2 | V2 压缩请求不可区分 | `server.rs:400-410` 只有 `responses`/`responses_compact`；V2 走 `responses` | 请求日志无法统计压缩频率、耗时、失败率 | 建议 |
| S3 | 第三方 Responses 线路无法声明支持远程压缩 | `commands.rs:1610-1637` 保存时沿用旧值；前端无开关 | 直连 OpenAI 兼容 Responses 中转的用户拿不到服务端压缩 | 建议 |
| S4 | 压缩结果无校验 | `responses.rs:1362-1385` 对 Compact 与 Create 同样处理；测试 `tests.rs:5096` 断言原样透传 | Codey 层不知道压缩是否产生了 `compaction` 项 | 建议：仅记录（`compactionItems`、`outputTokens`）不拦截 |
| S5 | `client_metadata` 原样发给第三方 | 测试 `tests.rs:5152` 断言 `keep` 字段被转发 | Codex 客户端元数据泄露给第三方中转 | 建议：非官方线路剥离非 Codey 键 |
| S6 | 删除消息时整段 `compacted` 快照被丢弃 | `message_delete.rs:689-696` | 删一条消息导致摘要丢失，Codex 需重建并可能再次压缩 | 建议：UI 提示，或只在被删轮次位于快照之后时保留 |
| O1 | 旧版 `/responses/compact` 为非流式，头超时 5 分钟 | `mod.rs:89`，`request_meta.rs:3-13` 不强制流式 | 用户界面长时间无反馈 | 可选：对 Compact 强制 `stream=true` 并聚合 |
| O2 | 无幂等键 | Codex 未提供；Codey 未生成 | 网络重试可能产生重复上游计费 | 可选：以 `thread-id + 输入哈希` 生成 `x-codey-compaction-key` 写日志 |
| — | 重复/并发压缩 | Codex 单轮内串行；子代理各自独立；Codey 无共享状态 | 未发现问题 | — |
| — | 压缩覆盖原始上下文 | Codex 写 rollout `compacted` 并保留原 rollout 行；Codey 不写 | 未发现问题 | — |
| — | 敏感信息 | 请求日志无 body（`route_request_log.rs:143-210`）；错误文本脱敏（`errors.rs:18-48`） | 未发现问题 | — |
| — | 超时/重试 | 头 30 s（流式）/5 min（非流式）、读空闲 90 s；仅连接前失败重试一次（`responses.rs:1236-1268`） | 与项目“不重放”原则一致 | — |

关于“Token 计算不准确”：Codey 不计算，只映射。映射覆盖 `prompt_tokens/cached/reasoning`（Chat）与 `input+cache_read`（Anthropic）。Anthropic 的 `cache_creation_input_tokens` 未计入 `input_tokens`（`sse_anthropic.rs:162-168` 仅加 `cache_read`），Codex 记账会低估该轮上下文（待验证影响大小）。

---

## 7. 开源方案调研与对比

许可证均于 2026-09-08 通过 GitHub License API 核实；实现细节标注“源码核实”的已读取当前 main 分支文件，其余来自公开文档与既有认知（未逐行核实）。

| 项目 | 触发阈值 | 保留内容 | Token 计算 | 摘要模型 | 失败处理 | 模型切换 | 许可证 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| openai/codex（源码核实） | `min(config, window×0.9)`，或用量 ≥ window×percent；scope 可选 total/body_after_prefix | 本地：用户消息 ≤20k + 摘要；远程 V2：user/assistant ≤64k + 服务端 compaction 项；丢工具项与 reasoning | 上游 usage，缺失时字节估算 | 同主模型；切换时先用旧模型，Codex 后端下可用新模型兜底 | 超限删最旧项重试；其他退避 ≤4；工具输出占位截断 | comp_hash / ModelDownshift 预压缩 | Apache-2.0 |
| Roo-Code（源码核实） | `autoCondenseContextPercent` 5–100 %；`condensed_recently` 防抖 | 首条消息 + 摘要 + 最近 N 条 | 上游 usage / 估算 | 可指定独立 condensing handler 与自定义提示 | 摘要后 token 未下降视为错误并回滚 | 由用户切换 handler，历史为明文 | Apache-2.0 |
| block/goose（源码核实） | `GOOSE_AUTO_COMPACT_THRESHOLD` 默认 0.8 | 摘要 + 工具对可单独摘要（`summarize_tool_call`） | provider 侧 `count_context_tokens` | 同 provider | 阈值 ≤0 或 ≥1 关闭；失败返回错误 | `get_context_limit(provider, model)` 按模型取窗口 | Apache-2.0 |
| Aider ChatSummary（源码核实） | 历史 > `max_tokens`（默认 1024） | 尾部约一半预算的消息保留，头部递归摘要（深度 ≤3），切点必须落在 assistant 之后 | tiktoken 逐条 | weak model | 递归失败则整体摘要 | 无特殊处理 | Apache-2.0 |
| langmem `summarize_messages`（源码核实） | `max_tokens_before_summary` | `RunningSummary` 增量更新；保证 AI 与其 tool 消息同组 | 可注入 `token_counter` | 任意 LLM | `max_summary_tokens < max_tokens` 校验 | 明文摘要，天然可移植 | MIT |
| OpenHands LLMSummarizingCondenser（源码核实） | 事件数 > `max_size`（240），目标 `max_size/2` | `keep_first`（2）永不摘要；遗忘事件 LLM 摘要成 Condensation 事件 | 事件计数（非 token） | 可用独立 `agent_llm` | 遗忘比例过高视为错误 | 明文事件 | MIT |
| LangChain `trim_messages` | 硬 token 上限 | 保留首 system/最近消息，`start_on=human` 保证角色合法 | `count_tokens_approximately` 或模型计数器 | — | 无摘要，纯裁剪 | — | MIT |
| LlamaIndex `ChatSummaryMemoryBuffer` | token_limit | 最近消息 + 累积摘要 | tiktoken | 可指定 | 无 | — | MIT |
| Semantic Kernel `ChatHistorySummarizationReducer` | target/threshold 消息数 | 首条 system + 摘要 + 最近 N | 消息数 | 同 kernel service | 失败返回原历史 | — | MIT |
| Letta | 上下文溢出（provider 错误）触发 | core memory + 递归摘要 | tiktoken | 同 agent 模型 | 溢出重试 | 明文 | Apache-2.0 |
| LiteLLM | — | — | `token_counter` 多 tokenizer；`model_prices_and_context_window.json` 维护逐模型窗口 | — | — | `get_max_tokens(model)` | 自定义（MIT + enterprise 目录，SPDX NOASSERTION） |
| Claude Code / Agent SDK | 约 95 % 自动 compact（官方文档） | 系统提示 + 摘要 + 近期内容；支持 `/compact 指令` | 服务端 | 同模型 | 失败提示手动 | — | Claude Code 非开源；SDK MIT |
| Anthropic API compaction beta | 服务端触发阈值 | 服务端摘要块 | 服务端 | 服务端 | — | 摘要块跨同族模型可用（文档说明，未核实） | — |

### 对 Codey 的结论

**可直接借鉴**

- LiteLLM 的“逐模型窗口表”思路：Codey 已有逐线路模型表，只缺窗口字段；不引入 LiteLLM 本身（Python，且许可证含企业目录），只借鉴数据形态。
- Roo-Code 的两项护栏：`condensed_recently` 防抖与“摘要后上下文未缩小即回滚”，对应 4.5 的防抖与预算校验。
- langmem 的 `max_summary_tokens < max_tokens` 前置校验，对应 3.2 的阈值校验。
- goose 的 `get_context_limit(provider, model)` 抽象：Codey 的 `RouteTarget` 应能回答“这条线路这个模型的窗口是多少”，供请求日志与提示使用。

**适合改造后采用**

- Codex 自身的本地压缩提示词与保留策略（Apache-2.0）：CodeyLocal 模式直接复用 `SUMMARIZATION_PROMPT` 语义与“用户消息 ≤ 20k + 摘要”的保留规则，减少与 Codex 行为差异。注意 Codey 不能访问 Codex 私有 crate，需要在 backend 内复制提示词文本并标注来源与许可证。
- Aider/langmem 的“工具调用与其输出必须同组保留或同组丢弃”规则，已在 `chat_tools.rs` 部分实现，CodeyLocal 摘要请求需沿用。

**不建议采用**

- 在代理层实现完整的记忆系统（Letta、mem0、OpenHands condenser 管线）：与 Codex 的 rollout/记账双写，违反本项目“不批量改写历史”的边界。
- 用 LangChain/LlamaIndex 类库做 Token 计数：Codey 是 Rust 进程，且 Codex 已按 usage 记账，二次计数只会产生分歧。
- 依赖 Anthropic 服务端 compaction：Codey 的 Anthropic 线路只是把 Responses 转成 Messages，Codex 不理解 Anthropic 摘要块。

---

## 8. 推荐改造方案（按优先级）

### 必须修复

1. **M1 透传上下文超限语义**（`local_router/errors.rs`, `sse_responses.rs`, `downstream.rs`）
   - 流式下游：当上游错误 `code` 属于 `{context_length_exceeded, context_window_exceeded, prompt_too_long, invalid_request_error+message 含 "context"}`（Anthropic 为 `invalid_request_error` + “prompt is too long”），写 `response.failed{error:{code:"context_length_exceeded", message}}`。
   - 非流式下游：写 JSON `{"error":{"code":"context_length_exceeded","type":"invalid_request_error","message":…}}`，HTTP 400。**待验证**：Codex 对非 SSE 的 400 JSON 是否也识别该 code（`codex-api` 中只在 SSE 路径找到判断）。若不识别，则对 Compact/本地压缩请求强制上游流式（O1）以进入 SSE 路径。
2. **M2 逐模型上下文配置**（第 3 节）：`config.rs`、`model_catalog.rs`、`commands/models/{state,sync}.rs`、`useModelSelection.ts`、`ModelSection.tsx`、`App.types.ts`、`mockApi.ts`。
3. **M3/M4 compaction 项与触发器的线路守卫**（`responses.rs` 在 `take_codey_route_metadata` 之后新增 `guard_compaction_items(&resolved, &mut body)`）：
   - `compaction_trigger` 落到适配线路 → 走 CodeyLocal（4.2C）；
   - 非 Codey 前缀的 `compaction` 项发往非产生线路 → 409 `compaction_not_portable`；
   - Codey 前缀项 → 解码为文本消息。
   - 新增 `RouteBindings` 旁的 `CompactionOrigins: LruCache<cmp_id, provider_id>`（上限 4096），由压缩响应的 `response.output_item.added` 事件填充（`ObservedResponsesDownstream::observe_event` 已在观察事件，可复用）。

### 建议优化

4. **S1** 当 CodeyLocal 可用后，`runtime_supports_remote_compaction` 改为“存在至少一条原生支持线路”即广告 `openai`，适配线路的压缩由 Codey 兜底；`remote_compaction_transport_requires_restart` 相应放宽。
5. **S2** `ResponsesRequestKind` 新增 `CompactionV2`，在 `proxy_parsed_responses_inner` 检查 `input` 末项类型后设置；请求日志 `request_kind` 输出 `responses_compaction_v2`；RequestLogDialog 增加筛选。
6. **S3** 前端线路表单为 `openai_responses` 协议暴露“支持远程压缩”开关，保存路径 `commands.rs:1610-1637` 允许该字段随表单更新（仅 Responses 协议）。
7. **S4** 压缩响应观察：记录 `compaction_items`、`retained_items`、`output_tokens` 到请求日志；不拦截。
8. **S5** 非官方线路剥离 `client_metadata` 中非 Codey 键。
9. **S6** 删除消息时若 `compacted` 快照在被删轮次之后，在 UI 提示“将丢弃一次压缩摘要”。

### 可选增强

10. **O1** 对 `ResponsesRequestKind::Compact` 强制上游 `stream=true` 并聚合，缩短无反馈时间。
11. **O2** 压缩请求幂等键写入请求日志与上游头 `x-codey-compaction-key`。
12. 控制台线路卡显示“压缩方式：服务端 / Codex 本地 / Codey 本地”与最近一次压缩耗时。
13. 后台预热摘要（4.2 表中“后台异步”）仅作研究项。

### 涉及的模块、接口、数据结构、配置项汇总

| 类别 | 项 |
| --- | --- |
| 配置结构 | `CodeyConfig.model_context_by_provider`（新）、`ModelContextConfig`、`CompactionMode`；保留 `supports_1m_context_by_provider` 做迁移 |
| 目录生成 | `model_catalog::refresh_for_provider_with_capabilities(…, context_configs)`、`apply_context_config`、`prepare_cached_catalog_for_current_capabilities` |
| 路由 | `RouteTarget.supports_remote_compaction`（新字段，替代运行时全局判断）、`RouteTarget.context_window_for(model)`、`ResponsesRequestKind::CompactionV2`、`CompactionOrigins`、`guard_compaction_items`、`run_codey_local_compaction` |
| 错误 | `errors.rs::classify_context_window_error`、`write_upstream_http_error` 分支、`ResponsesStreamState::fail_with_upstream_code` |
| 命令/前端 | `save_selected_models(modelContextConfigs)`、`App.types.ts: ModelContextConfig`、`useModelSelection.ts`、`ModelSection.tsx`（每模型窗口输入 + 压缩方式下拉）、`RequestLogDialog.tsx` 筛选 |
| 日志 | `RouteRequestLogEntry.compaction: Option<CompactionSummaryLog>`（items、tokens、mode） |
| Codex 配置 | 不新写 `model_context_window`；文档说明用户手写该键的副作用 |

---

## 9. 测试方案

Rust 路由测试沿用 `backend/src/local_router/tests.rs` 的模式（本地 `TcpListener` 模拟上游 + `LocalRouter::start`）。

| 场景 | 测试要点 |
| --- | --- |
| 上下文超限（原生 SSE） | 上游返回 `response.failed{code:context_length_exceeded}` → 下游原样收到；请求日志 `upstream_error_summary` 含 code |
| 上下文超限（Chat 400 JSON） | 上游 400 `{"error":{"code":"context_length_exceeded"}}`，下游流式 → 收到 `response.failed` 且 code 保留；非流式 → JSON 400 保留 code |
| 上下文超限（Anthropic） | 上游 400 `invalid_request_error` “prompt is too long” → 归一化为 `context_length_exceeded` |
| 压缩失败 | CodeyLocal 摘要请求上游 5xx/超时/空文本 → `response.failed{codey_compaction_failed}`，无第二次上游请求 |
| 压缩后仍超限 | 同 thread 60 s 内两次 CodeyLocal → 第二次 `codey_compaction_loop` |
| 并发压缩 | 两个不同 thread 同时发 `compaction_trigger` → 各自独立成功，`CompactionOrigins` 分别记录 |
| 模型切换（同线路） | 压缩后切模型，Codey 前缀 compaction 项解码为文本发往同线路 |
| 模型切换（跨线路，真实不透明项） | 记录来源 A 后发往 B → 409 `compaction_not_portable`，不上游 |
| 模型切换（跨线路，Codey 载体） | A 产生 Codey 项 → 发往 Chat 线路 B 解码为 user 文本；发往 Anthropic 线路 B 保持 user/assistant 交替 |
| 旧线路缺失 | ModelDownshift 压缩请求携带已删除线路别名 → 404 且错误文案含“压缩” |
| 工具调用 | 历史含 `function_call`/`function_call_output`/`web_search_call(in_progress)` + trigger → CodeyLocal 摘要请求不含 tools 字段，输出不含工具项 |
| 远程服务超时 | 头超时 30 s（流式）与 5 min（非流式）分别触发 504，请求日志 `stage=response_headers` |
| 热更新窗口期 | 运行时 `openai` 名下热加新增 Chat 线路 → trigger 落到该线路时走 CodeyLocal 而非 400 |
| 目录生成 | `ModelContextConfig{128000, None, 95, None}` → 目录 `context_window=128000, max=128000, auto_compact=null`；阈值 ≥ usable 时保存被拒 |
| 迁移 | 旧配置 `supports1MContextByProvider` → 生成 1M/100 配置项且渲染目录 `context_window=1000000` |
| 可观测性 | V2 trigger 请求日志 `request_kind=responses_compaction_v2`，含 `compaction_items` |
| JS 合同测试 | `pnpm test:js` 覆盖注入脚本对新增 `context_window` 元数据的透传（现有 `model-whitelist-inject` 用例扩展） |

回归基线：现有 `responses_compact_restores_route_identity_and_proxies_the_window_unchanged`、`responses_v2_compaction_trigger_passes_through_the_native_route`、`responses_compact_uses_the_chat_conversion_pipeline`、`nonportable_responses_history_items_are_ignored_during_chat_fallback_conversion` 中，最后一个对 `compaction` 项“静默忽略”的断言需要按 M4 调整为“Codey 项解码、外来项拒绝”。

---

## 10. 假设与待确认问题

**假设**

- A1 “同步线路”= Codex→Codey→上游的请求转发线路（见第 0 节）。
- A2 Codex Desktop 内置 CLI 0.153.0 的行为与 2026-09-08 GitHub main 一致；`remote_compaction_v2` 默认开启。
- A3 用户接受 Codey 为适配线路生成明文摘要并以 Codey 自有载体存入 Codex 历史（rollout 中将出现 `codey1:` 前缀的 base64 明文摘要）。
- A4 第三方中转的 `/models` 不保证返回窗口字段，因此窗口配置以手工为主、同步为辅。

**待确认**

- Q1 Codex 对非 SSE 的 HTTP 400 JSON 错误是否识别 `context_length_exceeded`（决定 M1 是否必须配合 O1）。需要抓一次真实 Codex 请求或阅读 `codex-api` HTTP 非流式路径。
- Q2 M4 的 409 拒绝是否可接受，还是希望 Codey 静默丢弃并注入“上下文已丢失”的提示消息？两者用户体验不同。
- Q3 修改模型目录后 Codex 是否需要重启才对已打开线程生效（影响窗口配置是否能热更新）。现有 1M 开关的处理方式（`state.rs` 视为目录字段）可作为参考，但未在本次验证。
- Q4 是否允许 Codey 在 CodeyLocal 模式下使用与主模型不同的“摘要模型”（例如同线路更便宜的模型）？Roo/Aider 支持此做法，但会引入第二个模型选择与额度归属问题。
- Q5 `client_metadata` 剥离是否会影响某些中转的计费/路由（S5 需要与实际中转确认）。
- Q6 Anthropic `cache_creation_input_tokens` 是否应计入 `input_tokens`（影响 Codex 记账精度）。
