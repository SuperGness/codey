# 构建与验证

需要 Rust stable、Python 3.11+；mock E2E 需要 Node.js 22+。Windows 默认使用 MSVC 工具链及 Visual Studio C++ Build Tools；GNU 也可用，但需要相应 Rust target 与 MinGW-w64 linker。工具路径均由当前进程环境提供，不修改全局配置。

在仓库根目录运行：

```powershell
cargo fmt -p codey-plugin-antigravity-router -- --check
cargo check -p codey-plugin-antigravity-router --locked
cargo fmt --manifest-path examples/plugins/antigravity-router/proxy-rust/Cargo.toml -- --check
cargo check --manifest-path examples/plugins/antigravity-router/proxy-rust/Cargo.toml --locked
.\examples\plugins\antigravity-router\scripts\test.ps1
.\examples\plugins\antigravity-router\scripts\build-release.ps1
# GNU 构建替代上一行；脚本为 GNU 添加静态运行库链接参数
.\examples\plugins\antigravity-router\scripts\build-release.ps1 -Target x86_64-pc-windows-gnu -OutputDir .\dist\antigravity-gnu
```

`test.ps1` 支持 `-Cargo`、`-Python`、`-Node`；`build-release.ps1` 支持 `-Cargo`、`-Python`。`test.ps1` 先构建原生动态库，再运行配置/线路单元测试、实际 DLL ABI 测试、代理单元测试与 mock E2E；mock 使用独立临时账号、模型缓存、图片目录和随机端口，不需要真实 Google 登录。测试可从任意当前工作目录运行。PowerShell 5.1 与 7 均支持安装和运行脚本。

原生 crate 加入 Codey workspace，使用当前仓库 `codey-plugin-sdk`；代理保持独立 `[workspace]` 和 `Cargo.lock`。构建脚本检查所有退出码，既有输出目录不会覆盖。原生 `.codey-plugin` 仅含 `manifest.json`、`config.json`、入口动态库，使用仓库 `scripts/package-plugin.py` 打包。外层 ZIP 另含代理 EXE、运行脚本、许可证与 `source.zip`。源码 ZIP 是只包含原生 crate、当前 SDK 与代理的最小 workspace，可独立重新运行上述命令。

默认产物位于本例 `dist/windows-x64`；导入原生包必须同时运行代理，原生包内没有 sidecar EXE。便携包校验：

```powershell
python examples/plugins/antigravity-router/scripts/verify-native.py <安装包.codey-plugin>
& <解压目录>/scripts/verify-bundle.ps1
.\examples\plugins\antigravity-router\scripts\test-portable.ps1 -Bundle <解压目录>
```

二进制分发当前仅提供 Windows x64 打包脚本。Rust 源码和 ABI 测试支持按平台动态库名适配，但其他平台的打包、安装与真实 Google 行为仍需独立验证。真实登录受 Google 网络和账号限制，mock 测试不会验证账号资格或线上模型可用性。
