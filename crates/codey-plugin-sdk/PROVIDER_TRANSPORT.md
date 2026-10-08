# 供应商传输协议 v1

此协议供可信原生插件适配非标准上游。C ABI 仍为 v1，单次消息仍受 1 MiB 限制。插件须同时声明 `provider.route.v1`、`provider.transport.v1`、`provider.account.v1`；缺少任意一项都会拒绝安装。普通线路插件无需迁移。

## 线路与账号

标准 HTTP 线路的 `provider.describe` 可额外返回 `supportsWebsockets`、`supportsRemoteCompaction`、`supportsNativeWebSearch`（缺省均为 `false`）。声明为 `true` 仅适用于没有 `transport` 的 `openaiResponses` 线路；远程压缩须实现独立 Responses compact 契约，原生搜索须实现 Responses `web_search` 结果契约。声明不改变 C ABI v1；旧宿主会拒绝未知字段，插件应注明最低宿主要求。

可选 `modelContexts` 将已声明模型 ID 映射到 `contextWindow`、`autoCompactTokenLimit` 和可选 `reserveOutputTokens`。仅声明真实上游预算，未知窗口应省略。窗口为 1024 至 10000000，压缩阈值须为正且不超过窗口的 90% 和扣除输出预留后的有效窗口；用户预算覆盖优先。若与 `transport.models` 同时声明，同一模型预算必须一致，且不能新增输出预留。

宿主同步插件线路模型时会重新调用 `provider.describe`，固定实例并核对配置、归属和线路连接；只更新当前线路的模型、预算和能力。标准 HTTP 的实际 `/models` 目录决定成员，自定义 transport 使用新的描述目录。已删除的插件模型不会被旧手动标记保留；独立手动模型、用户能力开关及空选择保留。连接描述变化需要重新加载插件，刷新不会创建已删除的线路。

`provider.describe` 沿用线路描述，协议须为 `openaiResponses`，`headers` 须为空，并增加 `transport`：

线路也可以返回可选的 `modelReasoningEfforts`，将模型 ID 映射到允许的思考档位。宿主会把它作为线路能力上限，用户可配置范围内的档位并自定义上游取值，但不能借取值启用未声明的 `max` 或 `ultra`；缺省时保持原有自动适配。旧版宿主会拒绝描述中的未知字段，需升级宿主后才能启用带此字段的插件。

```json
{
  "accountEmail": "user@example.com",
  "models": {
    "example-model": {"contextWindow": 272000, "autoCompactTokenLimit": 180000}
  },
  "imageGeneration": true,
  "imageEdit": true
}
```

邮箱去除首尾空白并忽略大小写，须唯一匹配宿主已保存且有效的账号；找不到、重复或身份改变时拒绝请求，不回退默认账号。宿主复用账号刷新服务，并在刷新后重新校验绑定与令牌有效期，只交付 access token 与上游 account ID。默认账号同步 Codex 登录态，不主动轮换其刷新令牌，无法取得有效凭据时要求更新登录。插件不得记录或持久化凭据，宿主不交付 refresh token、凭据路径或任意账号查询接口。

模型预算只能引用线路声明的模型，上下文范围为 1024 至 2000000，自动压缩预算须为正且不大于上下文。同步模型直接使用插件声明，用户预算覆盖优先。声明不证明上游容量。传输线路不能脱离插件或添加密钥及自定义头；当前不支持线路代理、WebSocket、原生 Web Search 和独立远程压缩。

## 调用与帧

管理接口拒绝 `provider.request.*`。同一实例的回调串行执行且须及时返回，网络等待应在后台完成。Rust 类型见 `transport.rs`，JSON 字段使用 camelCase。

1. `provider.request.start`：接受 requestId、accountEmail、operation、bodyBytes、credentials。operation 为 responses、image_generation 或 image_edit；credentials 只有 accessToken 和 upstreamAccountId。正文最多 64 MiB，插件须验证邮箱、标识、大小及并发容量。
2. `provider.request.write`：接受 requestId、base64 data、布尔值 finish。每块解码后最多 64 KiB，总量须等于 bodyBytes；空正文也有结束块。finish 后开始后台请求，成功返回空对象。
3. `provider.request.read`：接受 requestId，立即返回一帧，无内容时返回 pending，不在回调中等待上游。
4. `provider.request.cancel`：接受 requestId，幂等取消对应上传或后台任务并释放缓存，返回空对象。
5. `provider.request.stop`：接受空对象，幂等清理该实例的全部请求。宿主停用旧实例时发送，停用后仅允许此清理调用。

| type | 其他字段 | 约束 |
| --- | --- | --- |
| pending | 无 | 尚无可读内容 |
| headers | status、headers | 先于 data，状态码 200–599 |
| data | data | base64，解码后 1–65536 字节 |
| end | 无 | 正常结束 |
| error | code | 固定错误码，不含请求或凭据 |

headers 至多 32 项，单值最多 8192 字节；宿主只转发 content-type、cache-control、retry-after、x-request-id，不接受 Cookie、重定向或传输编码。插件输出解码后的 JSON 或标准 Responses SSE，累计正文最多 64 MiB。回调错误与 error 帧只保留 SDK 定义的公开错误码；响应头下发前按错误类别返回状态码，下发后仅终止流，不伪造成功，也不自动重放工具请求。插件自行过滤上游错误正文及诊断头，避免泄漏敏感信息。

## 清理与存储

线路先发布实例再登记，启停保护覆盖线路登记与释放；每次请求固定原生实例，重新启用不会使旧请求切换实例。客户端断开或超时后尽力 cancel；停用先撤销调度权限，再发送 stop。清理调用在没有当前异步运行时时使用后备执行器。插件须在 destroy 返回前停止并等待全部后台任务，不能只依赖结束通知或 stop。宿主不能强制结束失控原生线程。

插件自行限制并发、排队、响应大小、空闲和网络超时。业务历史使用 context.data_dir 内固定路径、私有权限及有界保留策略，并说明敏感数据风险。权限声明和目录约定均不构成沙箱。
