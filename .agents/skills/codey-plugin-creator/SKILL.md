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
- 目标是 `.codex-plugin/plugin.json`、Codex marketplace、skills、apps 或 MCP 清单，改用通用 `plugin-creator` 或对应专用技能。
- 用户只说创建插件但未说明类型时，先查看当前仓库和已有文件；仍无法判断再询问，不要把两套清单格式混用。

## 默认工作流

1. 读取仓库根目录、`../../../crates/codey-plugin-sdk/README.md`、相关协议参考和现有示例；确认目标平台、架构、插件 ID、版本、能力和配置字段。
2. 使用 `scripts/create_codey_plugin.py` 生成最小 crate，或在已有 crate 上增量修改；不要覆盖用户文件，除非用户明确要求 `--force`。
3. 实现 `Plugin::create` 和 `Plugin::invoke`，通过 `codey_plugin_sdk::export_plugin!` 导出入口。初始化时校验配置，方法名使用明确的命名空间，未知方法返回错误。
4. 只声明实际实现的能力：普通管理方法不需要 capability；请求生命周期使用 `request.lifecycle.v1`，认证上下文额外声明 `request.lifecycle.auth`；线路描述使用 `provider.route.v1`；查询任务数量使用 `appserver.call.v1`。
5. 将持久状态写入 `PluginContext.data_dir` 的固定文件名，日志使用 `context.log`；限制文件大小、拒绝调用方提供的任意路径，不写入凭据、请求正文或认证上下文。
6. 运行 `cargo fmt --check`、`cargo check` 和适合的 `cargo test`，再使用仓库的 `../../../scripts/package-plugin.py` 生成 `.codey-plugin`。打包前确认动态库、平台、架构和配置模板匹配。
7. 交付前检查安装包只包含 `manifest.json`、`config.json` 和入口动态库，校验 SHA-256、配置是 UTF-8 JSON 对象且不超过 1 MiB；说明导入后默认停用，需要用户显式启用。

## 脚手架与打包

从技能根目录运行：

```bash
python3 scripts/create_codey_plugin.py my-plugin \
  --path /path/to/plugins/my-plugin \
  --display-name "我的插件" \
  --capability request.lifecycle.v1
```

脚手架会创建 `../../../Cargo.toml`、`src/lib.rs`、`config.json` 和 `README.md`，并打印构建与打包命令。若插件位于仓库 `examples/plugins/<name>`，默认 SDK 相对路径可直接使用；其他位置通过 `--sdk-path` 指定 SDK 路径。

典型打包命令如下，动态库扩展名按平台替换：

```bash
cargo build --manifest-path /path/to/plugins/my-plugin/Cargo.toml
python3 scripts/package-plugin.py \
  --library /path/to/target/debug/libmy_plugin.dylib \
  --config /path/to/plugins/my-plugin/config.json \
  --output /tmp/my-plugin.codey-plugin \
  --id dev.codey.my-plugin --name "我的插件" --version 0.1.0
```

若使用请求生命周期能力，必须同步传入 `--capability request.lifecycle.v1`；请求头、响应头、认证上下文和等待策略不能脱离该能力单独声明。需要高级协议细节时读取 [references/native-plugin-spec.md](references/native-plugin-spec.md)。

## 能力选择

- 普通插件：实现自定义管理方法，不声明 capability；适合配置、持久化、健康检查和本地业务。
- 请求生命周期：仅修改已声明的请求头，严格遵守阶段允许的动作；不要尝试修改正文、URL、认证头或流式正文。
- 线路描述：返回固定的 `name`、`baseUrl`、`upstreamProtocol` 和有限模型列表；不要在插件中实现传输或保存密钥。
- 任务数量：声明 `appserver.call.v1` 后发送 `{"schema":"codey.appserver.v1","call":"codey://getTasks"}`，只使用结果里的 `running` 和 `failed`。
- 多能力插件：逐项验证宿主调度入口，避免把生命周期事件暴露为普通管理方法。

## 安全与兼容边界

- 原生插件不是沙箱；安装包导入阶段只检查文件，启用阶段才加载动态库。SHA-256 只证明包内容完整，不证明发布者身份。
- ABI 版本、平台、架构、入口路径和库哈希必须一致；不要手写哈希替代打包脚本。
- 配置编辑器只支持严格 JSON 对象；可用 `_comments` 提供说明，但插件收到配置前宿主会移除这些字段。
- 插件升级、停用、卸载和重启会保留 `data` 与 `logs`，配置修改后通常需要重新启用；不要假设卸载会自动清理数据。
- 需要超时、取消或后台任务时，确保任务在实例销毁前停止；原生死循环、段错误和进程退出无法由宿主隔离。

## 交付检查

- 说明生成的 crate、安装包路径、支持的平台和启用方式。
- 列出已运行的格式化、编译、单元测试和打包校验；无法运行的检查明确说明原因。
- 若用户要求发布、签名或分发，先确认目标渠道和信任模型；本技能默认只生成本地可验证安装包，不自动上传或安装到生产环境。
