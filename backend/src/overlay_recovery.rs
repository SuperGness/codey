//! Windows native overlay repair, independent of renderer CDP and Node Inspector.

use anyhow::Result;

pub(crate) async fn repair() -> Result<serde_json::Value, String> {
    tokio::task::spawn_blocking(repair_native)
        .await
        .map_err(|error| format!("浮窗恢复任务异常：{error}"))?
        .map(|message| serde_json::json!({ "status": "ok", "message": message }))
        .map_err(|error| format!("{error:#}"))
}

pub(crate) fn run_if_requested() -> Result<bool> {
    if std::env::args_os().nth(1).as_deref()
        != Some(std::ffi::OsStr::new("--repair-codex-overlays"))
    {
        return Ok(false);
    }
    let result = repair_native();
    #[cfg(windows)]
    rfd::MessageDialog::new()
        .set_title("Codey 浮窗恢复")
        .set_description(match &result {
            Ok(message) => message.clone(),
            Err(error) => format!("{error:#}"),
        })
        .set_level(if result.is_ok() {
            rfd::MessageLevel::Info
        } else {
            rfd::MessageLevel::Error
        })
        .show();
    result?;
    Ok(true)
}

#[cfg(not(windows))]
fn repair_native() -> Result<String> {
    anyhow::bail!("浮窗恢复仅支持 Windows 商店版 Codex")
}

#[cfg(windows)]
fn repair_native() -> Result<String> {
    use anyhow::Context;
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static LAST_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST_ATTEMPT
        .try_lock()
        .map_err(|_| anyhow::anyhow!("浮窗恢复正在进行"))?;
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(10)) {
        anyhow::bail!("请等待 10 秒后再恢复浮窗");
    }
    *last = Some(Instant::now());

    // Use an invocation-specific directory so concurrent helpers cannot replace
    // executable input. Keep the upstream cross-process mutex in the native code.
    let dir = codey_runtime_core::paths::default_app_state_dir()
        .join("overlay-recovery")
        .join(uuid::Uuid::new_v4().to_string());
    let result = (|| {
        crate::fs_util::atomic_write_private_with_parent(
            &dir.join("PetWindowAgent.cs"),
            include_bytes!("../resources/overlay-recovery/PetWindowAgent.cs"),
        )?;
        let source = dir
            .join("PetWindowAgent.cs")
            .to_string_lossy()
            .replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=New-Object System.Text.UTF8Encoding($false); Add-Type -TypeDefinition ([IO.File]::ReadAllText('{source}')); [PetWindowAgent]::RecoverOnce({})",
            std::process::id()
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let powershell = std::path::PathBuf::from(
            std::env::var_os("SystemRoot").context("找不到 Windows 系统目录")?,
        )
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = std::process::Command::new(powershell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
            ])
            .arg(encoded)
            .creation_flags(0x08000000)
            .output()
            .context("无法启动 Windows 浮窗恢复程序")?;
        if !output.status.success() {
            anyhow::bail!(
                "Windows 浮窗恢复程序失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        parse_result(&output.stdout)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(any(windows, test))]
fn parse_result(output: &[u8]) -> Result<String> {
    let result: serde_json::Value = serde_json::from_slice(output)?;
    if result["ok"] == true && result["restoredExactly"] == true {
        return Ok("已完成浮窗样式恢复，请拖动或点击浮窗确认效果。".to_string());
    }
    let reason = match result["code"].as_str().unwrap_or("") {
        "no_window" => "未找到可恢复的 Windows 商店版 Codex 浮窗，请先打开宠物或语音浮窗",
        "ambiguous_window" => "存在多个候选浮窗，请只保留需要恢复的一个浮窗",
        "input_busy" | "reset_interrupted" => {
            "鼠标按键、桌面切换或退出中断了恢复，请松开鼠标后重试"
        }
        "window_changed" => "浮窗尚未稳定或已重建，请稍后重试",
        "already_running" => "另一个浮窗恢复程序正在运行，请等待其退出",
        _ => "浮窗恢复未完成或未能确认原始样式已还原，请关闭并重新打开浮窗",
    };
    anyhow::bail!(
        "{reason}（{}）",
        result["code"].as_str().unwrap_or("invalid_result")
    )
}

#[cfg(test)]
mod tests {
    use super::parse_result;

    #[test]
    fn recovery_requires_verified_restoration() {
        assert!(parse_result(br#"{"ok":true,"restoredExactly":true}"#).is_ok());
        for response in [
            br#"{"ok":true}"#.as_slice(),
            br#"{"ok":true,"restoredExactly":false}"#,
            br#"{"ok":false,"restoredExactly":true,"code":"reset_interrupted"}"#,
            br#"{"ok":false,"code":"ambiguous_window"}"#,
            b"invalid JSON",
        ] {
            assert!(parse_result(response).is_err());
        }
    }
}
