# 许可与来源

原生插件（`src/`、`tests/native_abi.rs`、`Cargo.toml`）及 Codey SDK、仓库打包工具采用 **AGPL-3.0-only**，见本目录 `LICENSE` 与仓库许可证。

Rust 代理、mock E2E、便携脚本和配套文档采用 **MIT**，见 `proxy-rust/LICENSE` 与 `scripts/LICENSE`。代理从 [Rahularya01/pi-antigravity](https://github.com/Rahularya01/pi-antigravity) 的 main/v0.9.0（`a3d8caba1b10263420060406de57112ce16490d0`）迁移，保留原作者署名。整个插件包不能统一标为 MIT。

便携包包含实际使用的 Codey SDK 与插件对应源码、锁文件、构建脚本和依赖许可证；GNU 构建另附 GCC Runtime Library Exception 与 MinGW-w64 声明。Cargo 按锁文件获取第三方源码。依赖声明在打包时从当前构建图生成，校验和只证明完整性，不证明发布者身份。

Google Antigravity 账号、模型、配额和协议由 Google 控制；此项目不是 Google 官方插件。发布源码和二进制不内置 OAuth 应用凭据，登录与刷新需要通过 `ANTIGRAVITY_CLIENT_ID` / `ANTIGRAVITY_CLIENT_SECRET` 配置已获授权且支持相应 scope 的 Google OAuth 客户端；已有 refresh token 必须与签发它的客户端匹配。账号资格和自定义客户端是否被 Google 接受需单独确认。不得把客户端凭据、用户令牌或账号文件放进发布包。
