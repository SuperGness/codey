use std::path::Path;
use std::process::Command;

fn main() {
    for path in [
        "../src",
        "../index.html",
        "../vite.config.ts",
        "../vite.overlay.config.ts",
        "../package.json",
        "../public",
        "icons/Codey.ico",
        "../scripts/build-web.mjs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let status = Command::new(npm)
        .args(["run", "vite:build"])
        .current_dir(Path::new(".."))
        .status()
        .expect("无法运行 npm 构建 Codey Web 配置页");
    assert!(status.success(), "Codey Web 配置页构建失败");

    embed_windows_icon();
}

#[cfg(windows)]
fn embed_windows_icon() {
    winres::WindowsResource::new()
        .set_icon("icons/Codey.ico")
        .compile()
        .expect("无法嵌入 Codey Windows 图标");
}

#[cfg(not(windows))]
fn embed_windows_icon() {}
