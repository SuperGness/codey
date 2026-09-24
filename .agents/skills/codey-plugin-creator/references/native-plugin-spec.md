# Codey 原生插件参考

仅在实现对应能力时读取本文件；基础插件不需要加载全部协议细节。

## 包结构

`.codey-plugin` 是 ZIP，根目录必须有：

- `manifest.json`
- `config.json`
- `lib/<动态库文件>`

`manifest.json` 的核心字段为 `id`、`name`、`version`、`abiVersion: 1`、`platform`、`arch`、`entry`、`librarySha256`、`capabilities` 和 `headerNames`。生命周期插件还可以有 `responseHeaderNames`、`lifecycleFailurePolicy` 和 `lifecycleMaxWaitMs`。

平台值为 `macos`、`windows`、`linux`；架构沿用 Rust 名称，例如 `aarch64`、`x86_64`。入口必须是安全的包内相对路径，库哈希由 `../../../../scripts/package-plugin.py` 计算。

## SDK 约定

插件 crate 应使用 `crate-type = ["cdylib"]`，依赖 `codey-plugin-sdk`，实现：

```rust
impl Plugin for MyPlugin {
    fn create(config: Value, context: PluginContext) -> Result<Self, String>;
    fn invoke(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

codey_plugin_sdk::export_plugin!(MyPlugin);
```

同一实例的调用由 SDK 串行化。输入输出消息有 1 MiB 上限，初始化输入只允许 `config` 和 `context`。宿主会把 panic 转成错误，但不能隔离段错误、死循环或其他进程级破坏。

## 生命周期能力

使用 `request.lifecycle.v1` 后，宿主可调用 `request.beforeSend`、`request.afterHeaders`、`request.resume`，并发送 `request.completed`、`request.failed`、`request.cancelled`。前两个阶段允许的动作不同：

- `beforeSend`：`continue`、`wait`、`abort`，可修改 manifest 声明的请求头。
- `afterHeaders`：`continue`、`wait`、`retry`、`abort`；已发出的请求不能修改响应头。
- 结束事件：只做尽力通知，不依赖其完成关键清理。

默认异常策略是 `abort`；需要跳过当前插件时显式设置 `continue`。等待上限为 1 到 600000 毫秒，头名单各最多 32 项且不能重复。`request.lifecycle.auth` 只在同时声明 `request.lifecycle.v1` 时有效，凭据只应在内存中短暂使用。

## 线路能力

声明 `appserver.call.v1` 后，插件发送 `{"schema":"codey.appserver.v1","call":"codey://getTasks"}` 查看正在运行和失败的任务数量。未列入该 schema 的调用不会执行。

声明 `provider.route.v1` 后，宿主在启用时调用 `provider.describe`。返回对象至少包含一个模型，协议只能使用 `openaiResponses`、`openaiChatCompletions` 或 `anthropicMessages`。线路名最多 15 个字符；头部不能携带密钥，密钥由用户在线路配置中填写。

## 配置、数据与日志

`config.json` 必须是 UTF-8 JSON 对象，最大 1 MiB。`_comments` 可以出现在任意对象层级，值必须是字符串说明；宿主传给插件前会移除它们。插件数据写入 `context.data_dir`，日志通过 `context.log`，固定文件名并自行限制业务数据大小和并发更新。

## 本地验证

优先执行：

```bash
cargo fmt --check
cargo check --manifest-path <plugin>/Cargo.toml
cargo test --manifest-path <plugin>/Cargo.toml
python3 scripts/package-plugin.py ...
```

安装包导入后默认停用。启用前复核来源、动态库平台架构、配置权限和插件日志，不把完整性哈希当作签名验证。
