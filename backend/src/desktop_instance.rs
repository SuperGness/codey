//! One Codey desktop instance per Windows session. Codey stops every running
//! Codex before launching its own, so a second instance would kill the Codex
//! the first one is still starting and both launches would fail.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// Covers an instance that is still finishing its shutdown, such as right
/// after its startup error dialog was closed.
const HANDOFF_WAIT: Duration = Duration::from_secs(5);
const HANDOFF_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) struct DesktopInstanceGuard(Option<HANDLE>);

impl Drop for DesktopInstanceGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

enum Claim {
    Owned(DesktopInstanceGuard),
    Held,
}

fn try_claim() -> Claim {
    match unsafe { CreateMutexW(None, false, w!("Local\\Codey.Desktop")) } {
        Ok(handle) if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS => {
            let _ = unsafe { CloseHandle(handle) };
            Claim::Held
        }
        Ok(handle) => Claim::Owned(DesktopInstanceGuard(Some(handle))),
        Err(error) => {
            // The guard only prevents launch races; it must never keep Codey
            // from starting on its own.
            record_handoff("mutex_unavailable", Some(format!("{error:#}")));
            Claim::Owned(DesktopInstanceGuard(None))
        }
    }
}

/// Returns `None` when another instance owns this session. The caller must
/// then exit without touching Codex.
pub(crate) fn claim() -> Option<DesktopInstanceGuard> {
    let deadline = Instant::now() + HANDOFF_WAIT;
    let mut first_check = true;
    loop {
        if let Claim::Owned(guard) = try_claim() {
            return Some(guard);
        }
        let last_check = Instant::now() >= deadline;
        if (first_check || last_check) && crate::launcher::activate_visible_windows_codex_window() {
            record_handoff("activated_existing_codex", None);
            return None;
        }
        if last_check {
            break;
        }
        first_check = false;
        std::thread::sleep(HANDOFF_POLL_INTERVAL);
    }
    record_handoff("existing_instance_busy", None);
    let _ = rfd::MessageDialog::new()
        .set_title("Codey 已在运行")
        .set_description(
            "Codey 正在启动或运行 Codex，为避免中断当前启动，本次打开已取消。\n\n请等待 Codex 窗口出现；如果长时间没有出现，请在任务管理器中结束 Codey 后重新打开。",
        )
        .set_level(rfd::MessageLevel::Info)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    None
}

fn record_handoff(outcome: &str, detail: Option<String>) {
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.desktop_instance_handoff",
        serde_json::json!({ "outcome": outcome, "detail": detail }),
    );
}
