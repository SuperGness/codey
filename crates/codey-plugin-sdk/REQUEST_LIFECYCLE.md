# 请求生命周期协议 v1

此协议通过现有 ABI v1 的 `Plugin::invoke` 传递 JSON，宿主负责执行流程，插件负责自己的业务任务。适用于本地路由完成请求解析、线路选择和认证后的 Responses 请求及远程压缩；此前的输入校验失败不进入生命周期。

## 权限声明

在插件 manifest 中声明：

```json
{
  "capabilities": ["request.lifecycle.v1"],
  "headerNames": ["x-example-state"],
  "responseHeaderNames": ["x-example-state", "content-type"],
  "lifecycleFailurePolicy": "abort",
  "lifecycleMaxWaitMs": 30000
}
```

`headerNames` 控制可读取和修改的请求头；`responseHeaderNames` 控制可读取的响应头，各最多 32 项且名称不能重复。认证、Cookie 和宿主内部头不能通过这两个列表授权。响应头只读，可声明读取 Content-Type 等传输信息。空名单有效；请求体、URL和路由不能由此协议修改。

失败策略默认为 `abort`：回调错误、无效动作、实例停用、执行忙或超时会终止本次请求。选择 `continue` 时跳过本阶段出错的插件；插件主动返回的 `abort` 始终生效。等待上限默认为 30000 毫秒，允许 1–600000。这些字段及 `request.lifecycle.auth` 都要求同时声明 `request.lifecycle.v1`。

需要官方账号认证上下文的可信插件可额外声明 `request.lifecycle.auth`。仅在官方线路且已解析到令牌时，控制事件带 `credentials: {accessToken, upstreamAccountId}`；其他情况下省略。该信息只经过后端内存，插件不得放入错误消息、管理方法输出或日志。原生插件本身并不处于沙箱中。

需要为非官方原生 Responses 线路轮换 Key 的可信插件必须同时声明 `request.lifecycle.v1`、`request.lifecycle.api_key` 与非空 `apiKeyUrls`。该列表最多 32 个精确 HTTPS endpoint，回环 mock 允许 HTTP；路径仅允许 `/responses`、`/responses/compact` 及其 `/v1` 前缀形式。禁止通配符、用户信息、query、fragment、重复规范 URL 和任意其他路径。旧 manifest 不声明该能力时默认空列表；列表与能力缺一会被拒绝。此权限独立于官方认证上下文，普通头名单始终不能授权 Authorization。

`request.lifecycle.turn_state` 需同时声明生命周期能力，允许宿主使用另一个已保存官方账号发起独立预请求，再安装状态头和路由 Cookie；插件不接收来源凭据或 Cookie。仅适用宿主原生 HTTP Responses、官方账号和固定的 `https://chatgpt.com/backend-api/codex/responses` 地址，不支持自定义网关、压缩或传输插件。宿主仅向声明此能力的控制事件提供 `metadata.turnStateAuthorized`。

## 事件与顺序

每个逻辑请求固定一份已启用插件实例快照，按插件 ID 顺序调用。运行中启用的新实例只参与后续请求；停用或替换会撤销旧实例在等待中的控制权。同一个实例串行执行回调，正常并发请求异步排队，等待期间不占用原生线程。

| 方法 | 时机 | 返回值 |
| --- | --- | --- |
| `request.beforeSend` | 每次上游 HTTP 发送前 | `continue`、`wait`、`abort`、已授权的 `borrowTurnState` |
| `request.afterHeaders` | 收到上游响应头，下游响应尚未开始 | `continue`、`wait`、`retry`、`abort` |
| `request.resume` | 宿主轮询同一插件的等待任务 | 沿用原阶段允许的动作 |
| `request.completed` | HTTP 响应成功传输完成 | 忽略 |
| `request.failed` | HTTP 错误、传输失败或插件终止 | 忽略 |
| `request.cancelled` | 可检测的下游取消或请求任务被丢弃 | 忽略 |

请求扩展只有这一条调度路径。`request.beforeSend` 是事件名，manifest 中的能力名统一为 `request.lifecycle.v1`。上表方法不能通过 `invoke_codey_plugin` 调用，避免管理请求伪造生命周期事件或占用实例执行权。

控制事件可用 `codey_plugin_sdk::lifecycle::RequestEvent` 解析：

```json
{
  "requestId": "request-42",
  "stage": "afterHeaders",
  "attempt": 0,
  "metadata": {
    "requestId": "request-42",
    "routeId": "route-example",
    "officialAccountId": null,
    "officialAccountEmail": null,
    "upstreamAccountId": null,
    "accountType": null,
    "requestedModel": "model-alias",
    "model": "actual-model",
    "protocol": "OpenAI Responses",
    "stream": true,
    "requestKind": "responses",
    "subagent": false
  },
  "headers": {"x-example-state": "old"},
  "response": {"status": 200, "headers": {"x-example-state": "new"}}
}
```

`requestId` 在整个逻辑请求内不变，`attempt` 首次为 0，重发时为 1；`stage` 仅为 `beforeSend` 或 `afterHeaders`，轮询仍使用原阶段。`response` 在发送前为 null。请求模型是最终上游模型名。`officialAccountId` 是 Codey 的稳定本地账号 ID，`upstreamAccountId` 是实际上游身份，不能混用。`officialAccountEmail` 是官方线路关联的本地账号记录中的登录邮箱，随路由快照读取，不接受客户端请求头提供的邮箱；非官方线路或记录缺少邮箱时为 null。`accountType` 仅在官方令牌有套餐声明时提供；缺失值为 null，插件应自行判断能否处理。`requestKind` 为 `responses` 或 `responses_compact`。

仅声明 API Key 能力的当前插件会收到宿主建立的 `metadata.upstreamUrl`、`officialAccount`、`apiKeyAuthorized` 与 `apiKeySelected`。URL 来自实际 HTTP 目标解析；授权要求精确匹配 scope、非官方线路和宿主原生 Responses HTTP 传输（不含 provider.transport.v1）。`apiKeySelected` 仅表示当前插件已为该逻辑请求选定 Key，不携带 Key 值；重发时为 true，插件应直接返回 continue。普通插件与终态事件不接收这些标记。

## 控制动作

使用 `codey_plugin_sdk::lifecycle::Action` 构造结果，或返回对应 JSON：

```json
{"action":"continue","headers":[{"name":"x-example-state","value":"ready"}]}
```

发送前可修改声明过的请求头，`value: null` 表示删除；后面的插件能看到已通过校验的修改。请求头值不能包含控制字符或 DEL，单项不超过 16384 字节，一次修改合计不超过 32768 字节。无法写入 HTTP 头的修改会拒绝本次动作，不会静默丢弃同一次已经通过校验的其他修改。响应头阶段的 `continue` 只能返回空头列表，不能修改已发出的请求。

已授权插件仅可在首次 `request.beforeSend` 的 `continue` 返回独立的 `apiKey` 字符串。值必须为非空可见 ASCII，最长 8192 字节。宿主在最终发送前安装敏感 Bearer HeaderValue，并移除竞争认证头；它不经过 HeaderPatch、插件广播或请求日志。同一插件实例的 SDK 回调互斥，Key 选择与轮询游标推进可在一个回调中完成。一个逻辑请求选定后固定，重试复用；再选、多个插件覆盖、未授权目标、官方线路、协议转换、响应头阶段及 resume 均拒绝，敏感校验失败不受 continue 失败策略放行。原生传输仍处理原始正文与 SSE，不提供转发网关或协议转换；上游客户端不跟随重定向。SDK Action 不实现 Debug，避免打印 Key。

```json
{"action":"borrowTurnState","targetAccountEmail":"target@example.com","sourceAccountEmail":"source@example.com","model":"gpt-5.4","timeoutMs":15000}
```

此动作仅允许首次 `request.beforeSend`，目标邮箱必须匹配当前官方账号，来源邮箱须唯一匹配另一个已保存账号。宿主发送独立的流式 ping，取得响应头后关闭流，仅提取 `X-Codex-Turn-State` 和有效的 `__cflb`、`__oailb` Cookie，发送前替换原有两个请求头；目标账号的认证信息保持不变。超时包含账号刷新和响应头等待，范围为 1000–30000 毫秒；失败终止原请求，不受 continue 策略放行。状态只存于当前逻辑请求，重试复用，结束释放；停用和可检测的下游取消会中断预请求。每个逻辑请求只能选择一次，不能与 API Key 动作混用。预请求增加来源账号用量和发送延迟，上游效果需自行确认。

```json
{"action":"wait","token":"job-7","pollAfterMs":100}
```

插件先创建自己的后台任务并迅速返回。宿主保留当前请求或未消费的响应，异步等待后调用同一实例的 `request.resume`，参数保留原事件并增加 `token`。默认轮询间隔为 100 毫秒，实际限制在 50–1000 毫秒。同一次等待不能更换 token；后续 `wait` 不刷新截止时间。token 最长 256 字节且不能包含控制字符，插件应结合 `requestId`、`stage` 和 `attempt` 查找任务，不能只凭 token 跨请求恢复。

```json
{"action":"retry","headers":[{"name":"x-example-state","value":"ready"}]}
```

仅响应头阶段可返回 `retry`。宿主丢弃当前响应，将头修改应用到下一次发送，再运行各插件的 `beforeSend`。正文复用原始编码字节；宿主既有的协议修复回退可调整其负责的正文，但与插件共享最多一次重发额度。达到额度或下游已经开始输出时拒绝重发。重发可能产生额外上游计费。

```json
{"action":"abort","status":502,"code":"resource_unavailable","message":"当前资源不可用"}
```

`status` 默认 502，只允许 400–599；`code` 默认 `plugin_aborted`，最长 128 字节；`message` 默认提示插件终止请求，最长 2048 字节。两者不能包含控制字符。动作严格校验，未知字段不会被静默忽略。

## 时间、取消与传输边界

每次回调的异步排队和原生执行共用最多 3 秒期限；同一插件阶段从首次 `wait` 开始按 manifest 的上限计时。每个逻辑请求所有插件、阶段及重试的累计回调和等待预算最多 600 秒。原生代码无法被强制终止；超时后执行权保持占用直到该回调返回，后续请求只在异步层有限等待，不继续堆积阻塞任务。

结束事件可用 SDK 的 `TerminalEvent` 解析，只包含元数据、最近阶段、attempt、可用的 token、最终 HTTP status 和错误 code，不携带凭证或头部值。每个请求只提交一次终态通知；通知在后台尽力执行，回调忙、线程异常或进程退出时可能无法送达。插件后台任务仍须有自己的超时，并在插件销毁时停止。

启用生命周期插件的请求使用 HTTP/SSE 上游，以便在每次逻辑响应开始前检查响应头；下游 WebSocket 及其历史恢复仍可使用。未启用生命周期插件时沿用原有原生 WebSocket 路径。v1 不提供正文或流式块修改，`completed` 只表示传输完成，不保证原生 SSE 正文中的业务任务成功。

WebSocket 下游关闭可中断等待。HTTP 的 FIN 与合法写半关闭无法区分，宿主不会将其直接作为取消；可检测的连接写入失败和请求任务丢弃会结束生命周期，其他等待受期限约束。

## 打包

打包脚本支持 `--capability request.lifecycle.v1`、重复的 `--header` 与 `--response-header`、`--lifecycle-failure-policy` 和 `--lifecycle-max-wait-ms`。头名单及生命周期参数必须显式搭配生命周期能力，缺失时直接报错。需要认证上下文时另加 `--capability request.lifecycle.auth`；需要选取 API Key 时另加 `--capability request.lifecycle.api_key` 及重复的 `--api-key-url` 精确授权地址，域名须使用 ASCII 或 Punycode，避免不同 IDNA 规则改变授权目标。安装包必须包含 `config.json`；打包时省略 `--config` 会生成空对象模板。

需要上述预请求能力时另加 `--capability request.lifecycle.turn_state`，无需声明认证能力或 Cookie 头名单；示例见 `examples/plugins/astra-turn-state`。此协议只提供请求控制，生命周期声明不会自动挂载插件界面。
