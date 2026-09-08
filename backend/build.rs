use std::path::Path;
use std::process::Command;

fn main() {
    for path in [
        "../src",
        "../vite.overlay.config.ts",
        "../package.json",
        "../pnpm-lock.yaml",
        "icons/Codey.ico",
        "../scripts/build-overlay.mjs",
        "../scripts/build-output.mjs",
        "../public",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    // scripts/build.mjs 已经先运行过 build-overlay.mjs，再由 release 构建脚本
    // 重复执行一次 Vite 只会白花几秒；调用方设置该变量表示产物已就绪。
    // include_str! 会把 dist-overlay 下的文件登记进 dep-info，跳过时改动产物
    // 仍会触发重新编译。
    println!("cargo:rerun-if-env-changed=CODEY_SKIP_OVERLAY_BUILD");
    if std::env::var_os("CODEY_SKIP_OVERLAY_BUILD").is_some_and(|value| value == "1") {
        assert!(
            Path::new("../dist-overlay/codey-overlay.js").is_file(),
            "CODEY_SKIP_OVERLAY_BUILD=1 但 dist-overlay/codey-overlay.js 不存在，请先运行 pnpm run vite:build"
        );
    } else {
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let status = Command::new(npm)
            .args(["run", "vite:build"])
            .current_dir(Path::new(".."))
            .status()
            .expect("无法运行 npm 构建 Codey Web 配置页");
        assert!(status.success(), "Codey Web 配置页构建失败");
    }

    #[cfg(windows)]
    embed_windows_icon();
}

#[cfg(windows)]
fn embed_windows_icon() {
    let mut resource = winres::WindowsResource::new();
    resource.set_icon("icons/Codey.ico").set_manifest(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>"#,
    );
    resource.compile().expect("无法嵌入 Codey Windows 图标");
}
