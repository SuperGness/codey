# Antigravity 插件

为 Codey 提供通过 Google Antigravity 使用 Gemini 和 Claude 的原生插件，以及管理 Google 登录的独立 Rust 代理。

- 支持流式对话、多轮工具调用、模型发现和多账号配额切换。
- 支持联网搜索与图片生成；实际可用模型和配额取决于 Google 账号。
- 原生插件拥有宿主进程权限，仅启用可信来源；Google 登录由代理管理，需自行配置已获授权的 OAuth 客户端。

使用方法见 [INSTALL.md](INSTALL.md)，从源码构建见 [BUILDING.md](BUILDING.md)。代理源自 MIT 许可的 [pi-antigravity](https://github.com/Rahularya01/pi-antigravity)；原生插件为 AGPL-3.0-only，完整声明见 [NOTICE.md](NOTICE.md)。
