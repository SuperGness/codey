---
name: codey-plugin-creator
description: Create, extend, package, and validate trusted native Codey Rust plugins that use the repository's codey-plugin-sdk and `.codey-plugin` installer format. Use for new Codey plugins, capability-specific plugin prototypes, configuration templates, request lifecycle or provider route integrations, and local package verification; do not use for generic Codex marketplace plugins or UI-only extensions.
metadata:
  short-description: 快速创建 Codey 原生插件
---

# Codey Plugin Creator

根据当前仓库的 Codey 原生插件系统，生成可编译、可打包、可验证的 Rust `cdylib` 插件。插件拥有宿主进程权限，默认只处理可信本地开发场景。

## 先判定插件类型

- 目标包含 `manifest.json`、动态库、`config.json`、`.codey-plugin`、`codey-plugin-sdk` 或 Codey 控制台导入，使用本技能。
- 目标是 `.codex-plugin/plugin.json`、Codex marketplace、skills、apps 或 MCP 清单，使用对应格式与可用的专用工具。
- 用户只说创建插件但未说明类型时，先查看当前仓库和已有文件；仍无法判断再询问，不要把两套清单格式混用。

## 默认工作流

1. 读取仓库根目录、`../../../crates/codey-plugin-sdk/README.md`（相对本文件所在目录）、相关协议参考及已有插件；确认目标平台、架构、插件 ID、版本、能力和配置字段。所有本地插件在仓库 `plugins/` 下开发，不提交源码、配置或产物到 Git；SDK、模板和 Skill 保留版本控制。
2. 使用 `scripts/create_codey_plugin.py` 生成最小 crate，或在已有 crate 上增量修改；不要覆盖用户文件，除非用户明确要求 `--force`。
3. 实现 `Plugin::create` 和 `Plugin::invoke`，通过 `codey_plugin_sdk::export_plugin!` 导出入口。初始化时校验配置，方法名使用明确的命名空间，未知方法返回错误。
4. 只声明实际实现的能力：请求生命周期使用 `request.lifecycle.v1`，认证上下文、API Key 选取、借用账号状态分别另加 `request.lifecycle.auth`、`request.lifecycle.api_key`、`request.lifecycle.turn_state`；线路描述使用 `provider.route.v1`，自定义传输另需 `provider.transport.v1` 和 `provider.account.v1`。
5. 将持久状态写入 `PluginContext.data_dir` 的固定文件名，日志使用 `context.log`；限制文件大小、拒绝任意路径，不记录凭据、请求正文或认证上下文。协议恢复确需保存工具历史时，按账号隔离、限制容量和保留时间，并说明敏感参数及卸载保留风险。
6. 先用 `cargo fmt --manifest-path <插件>/Cargo.toml` 格式化，再运行对应 crate 的格式检查、`cargo check` 和适合的 `cargo test`；使用仓库的 `../../../scripts/package-plugin.py` 生成 `.codey-plugin`。打包前确认动态库、平台、架构和配置模板匹配。
7. 交付前检查安装包只包含 `manifest.json`、`config.json` 和入口动态库，校验 SHA-256、配置是 UTF-8 JSON 对象且不超过 1 MiB；说明导入后默认停用，需要用户显式启用。

## 脚手架与打包

从技能根目录运行：

```bash
python3 scripts/create_codey_plugin.py my-plugin \
  --path /path/to/plugins/my-plugin \
  --display-name "我的插件" \
  --capability request.lifecycle.v1
```

脚手架默认在脚本所属仓库的 `plugins/<名称>/` 创建 `Cargo.toml`、`src/lib.rs`、`config.json` 和 `README.md`，并打印构建与打包命令。生成的 crate 使用独立 `[workspace]`，默认 SDK 路径按脚本所在仓库计算；`--sdk-path` 可指定相对插件目录的路径或绝对路径。已有目标文件会在写入前统一检查，默认拒绝覆盖。

典型打包命令如下，动态库扩展名按平台替换：

```bash
cargo build --manifest-path /path/to/plugins/my-plugin/Cargo.toml --target-dir /path/to/plugins/my-plugin/target
python3 ../../../scripts/package-plugin.py \
  --library /path/to/plugins/my-plugin/target/debug/libcodey_plugin_my_plugin.dylib \
  --config /path/to/plugins/my-plugin/config.json \
  --output /path/to/Desktop/my-plugin/my-plugin-macos-aarch64-0.1.0.codey-plugin \
  --id dev.codey.my-plugin --name "我的插件" --version 0.1.0
```

若使用请求生命周期能力，必须同步传入 `--capability request.lifecycle.v1`；请求头、响应头、认证上下文和等待策略不能脱离该能力单独声明。需要高级协议细节时读取 [references/native-plugin-spec.md](references/native-plugin-spec.md)。

构建命令生成当前平台的动态库。Windows 使用 `codey_plugin_my_plugin.dll`；指定 `--target` 时产物位于 `target/<目标三元组>/debug/`。打包参数 `--platform`、`--arch` 只描述现有库，不执行交叉编译。发布构建可加 `--release`，对应目录改为 `release/`；跨平台构建须先确认目标工具链和链接器可用。

交付具体插件时分别检查 macOS 与 Windows 的构建条件，产物放到当前用户桌面上的插件名称目录，缺失时创建；文件名包含平台、架构和版本，同名已存在时改用新文件名。无法实际构建的平台说明限制并提供可复现流程，不将检查通过写成构建完成。打包工具拒绝覆盖已有文件。仓库只有 SDK 或本次只调整开发规范时，不创建业务插件或正式安装包。

## 能力选择

- 普通插件：实现自定义管理方法，不声明 capability；适合配置、持久化、健康检查和本地业务。
- 请求生命周期：仅修改已声明的请求头，严格遵守阶段允许的动作；不要尝试修改正文、URL、认证头或流式正文。
- 线路描述：返回固定的 `name`、`baseUrl`、`upstreamProtocol` 和有限模型列表；普通线路的密钥由宿主管理。自定义传输须完整阅读 `../../../crates/codey-plugin-sdk/PROVIDER_TRANSPORT.md`，遵守邮箱绑定、有界传输、错误白名单和后台清理要求，不持久化凭据。脚手架的传输占位会明确失败，完成业务实现后才能作为可用插件交付。
- 任务数量：`appserver.call.v1` 仅为兼容声明；通过本地路由 `POST /codey/api/appserver` 发送 `Request::get_tasks()` 的 JSON，须携带路由 Bearer token。`PluginContext` 不注入地址或令牌，SDK 不提供网络客户端；不能仅靠 capability 完成调用。校验响应 schema 和 error 后读取 `running` 和 `failed`。
- 多能力插件：逐项验证宿主调度入口，避免把生命周期事件暴露为普通管理方法。

## 安全与兼容边界

- 原生插件不是沙箱；安装包导入阶段只检查文件，启用阶段才加载动态库。SHA-256 只证明包内容完整，不证明发布者身份。
- ABI 版本、平台、架构、入口路径和库哈希必须一致；不要手写哈希替代打包脚本。
- 配置编辑器只支持严格 JSON 对象；可用 `_comments` 提供说明，宿主传入插件前会移除。离线可用 SDK 的 `config::validate` 检查格式；已连接桥接时可用 `get_codey_plugin_host_info` 查询接受的声明和限制、`validate_codey_plugin_config` 预检查文本。两者不调用插件或授予权限，格式通过仍需插件验证业务约束。
- 插件升级、停用、卸载和重启会保留 `data` 与 `logs`，配置修改后通常需要重新启用；不要假设卸载会自动清理数据。
- 需要超时、取消或后台任务时，确保任务在实例销毁前停止；原生死循环、段错误和进程退出无法由宿主隔离。

## 交付检查

- 说明生成的 crate、安装包路径、支持的平台和启用方式。
- 列出已运行的格式化、编译、单元测试和打包校验；无法运行的检查明确说明原因。
- 若用户要求发布、签名或分发，先确认目标渠道和信任模型；本技能默认只生成本地可验证安装包，不自动上传或安装到生产环境。
