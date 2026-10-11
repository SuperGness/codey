# Codey 原生插件参考

仅在实现对应能力时读取本文件；基础插件不需要加载全部协议细节。

## 包结构

`.codey-plugin` 是 ZIP，根目录必须有：

- `manifest.json`
- `config.json`
- `lib/<动态库文件>`

`manifest.json` 的核心字段为 `id`、`name`、`version`、`abiVersion: 1`、`platform`、`arch`、`entry`、`librarySha256`、`capabilities` 和 `headerNames`。生命周期插件还可以有 `responseHeaderNames`、`lifecycleFailurePolicy` 和 `lifecycleMaxWaitMs`。

插件 ID 最多 96 字符，以小写字母开头，只含小写字母、数字及 `._-`，不能包含连续点或以点结尾。脚手架和打包器在写入前检查 ID；请求头声明不能包含认证、Cookie、宿主保留头或无效名称。

平台值为 `macos`、`windows`、`linux`；架构沿用 Rust 名称，例如 `aarch64`、`x86_64`。入口必须是安全的包内相对路径，库哈希由 `../../../../scripts/package-plugin.py`（相对本参考文件目录）计算。

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

默认异常策略是 `abort`；需要跳过当前插件时显式设置 `continue`。等待上限为 1 到 600000 毫秒，头名单各最多 32 项且不能重复。`request.lifecycle.auth`、`request.lifecycle.api_key` 和 `request.lifecycle.turn_state` 均依赖 `request.lifecycle.v1`；API Key 能力打包时还需 `--api-key-url` 精确授权 Responses 地址。凭据只应在内存中短暂使用。

## 线路能力

`appserver.call.v1` 仅为兼容声明，不授予 HTTP 权限。通过本地路由 `POST /codey/api/appserver` 发送 `{"schema":"codey.appserver.v1","call":"codey://getTasks"}`，并携带路由 Bearer token，才能查看正在运行和失败的任务数量。`PluginContext` 不提供地址或令牌，SDK 只提供协议类型；未列入该 schema 的调用不会执行。

声明 `provider.route.v1` 后，宿主在启用时调用 `provider.describe`。返回对象至少包含一个模型，协议只能使用 `openaiResponses`、`openaiChatCompletions` 或 `anthropicMessages`。线路名最多 15 个字符；头部不能携带密钥，密钥由用户在线路配置中填写。

需要自定义请求正文或流式协议时，同时声明 `provider.transport.v1` 与 `provider.account.v1`，在描述中提供 `transport.accountEmail`。宿主唯一匹配已保存账号并提供短期访问凭据，分块正文和帧协议沿用 ABI v1；管理方法不能调用 `provider.request.*`。优先使用 SDK 的 `Frame::headers` 和 `Frame::error` 构造响应头及错误帧，复用宿主校验与脱敏规则。实现前完整阅读仓库 `crates/codey-plugin-sdk/PROVIDER_TRANSPORT.md`，落实取消、停用及后台运行时销毁要求。

## 配置、数据与日志

`config.json` 必须是 UTF-8 JSON 对象，最大 1 MiB。`_comments` 可以出现在任意对象层级，值必须是字符串说明；宿主传给插件前会移除它们。SDK 的 `config::validate` 只返回格式诊断，`config::parse` 返回移除说明后的运行配置，与宿主安装、保存共用规则。已连接管理桥接时可用 `validate_codey_plugin_config` 预检查文本，用 `get_codey_plugin_host_info` 查询协议限制；两者不执行插件或提供额外权限。插件数据写入 `context.data_dir`，日志通过 `context.log`，固定文件名并自行限制业务数据大小和并发更新。

## 本地验证

优先执行：

```bash
cargo fmt --manifest-path <plugin>/Cargo.toml --check
cargo check --manifest-path <plugin>/Cargo.toml
cargo test --manifest-path <plugin>/Cargo.toml
python3 scripts/package-plugin.py ...
```

安装包导入后默认停用。启用前复核来源、动态库平台架构、配置权限和插件日志，不把完整性哈希当作签名验证。

跨平台构建先检查 `rustup target list --installed` 和目标链接器。在具备 Apple 工具链的 macOS 上使用 `cargo build --manifest-path plugins/<名称>/Cargo.toml --release --target aarch64-apple-darwin --target-dir plugins/<名称>/target`；Windows 原生构建可使用 `x86_64-pc-windows-msvc`，已配置 MinGW 的环境可用 `x86_64-pc-windows-gnu` 并设置 `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER`。仅安装 Rust target 不代表链接工具齐备；交叉编译成功也不代表已在 Windows 宿主验证运行。

对应动态库位于 `target/<目标三元组>/release/`。使用匹配的 `--platform` 与 `--arch` 打包，输出到实际桌面目录下的 `<插件名称>/<名称>-<平台>-<架构>-<版本>.codey-plugin`；目录由打包器创建，已有文件会被拒绝。再次交付同一版本时添加新的文件名后缀，保留原产物。
