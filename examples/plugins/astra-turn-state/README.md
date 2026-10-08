# Astra Turn State 插件

为配置的降智账号借用非降智账号的 `X-Codex-Turn-State` 和路由 Cookie，宿主预请求成功后再发送原消息，保持降智账号的认证信息。

- 在 Codey 添加两个官方账号，导入插件后填写 `degradedAccountEmail` 和 `healthyAccountEmail`；`mintModel` 留空沿用消息模型，`timeoutMs` 控制预请求超时，保存配置后启用。
- 每条消息会额外消耗来源账号的 ping 额度并增加请求延迟，服务端最终效果取决于宿主和上游服务。
- 需要包含 `request.lifecycle.turn_state` 能力的新版 Codey；仅适用官方默认地址的 HTTP Responses，不处理压缩、自定义网关或第三方线路，预请求失败会停止发送。
- 本插件不保存凭据，也不创建网络线程；仅加载可信来源的原生插件，它拥有宿主进程权限。
- 插件日志会记录 `turn_state_requested`、`turn_state_minted`、`turn_state_applied` 或 `turn_state_failed`，用于确认流程是否执行。

流程参考 [spumon1/SUCK_MY_ASTRA](https://github.com/spumon1/SUCK_MY_ASTRA)，许可证见 `THIRD_PARTY_LICENSES.txt`，同时嵌入动态库的 `astra.status` 输出。

## 本地构建

在仓库根目录执行，以下动态库名称适用于 macOS，其他平台替换对应扩展名：

```sh
cargo build --release --manifest-path examples/plugins/astra-turn-state/Cargo.toml
python3 scripts/package-plugin.py --library examples/plugins/astra-turn-state/target/release/libcodey_plugin_astra_turn_state.dylib --config examples/plugins/astra-turn-state/config.json --output examples/plugins/astra-turn-state/dist/astra-turn-state-0.1.0-macos-aarch64.codey-plugin --id dev.codey.astra-turn-state --name "Astra Turn State" --version 0.1.0 --capability request.lifecycle.v1 --capability request.lifecycle.turn_state --lifecycle-failure-policy abort
```
