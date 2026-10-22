//! Codex launch command construction shared with the Codey backend.
//!
//! The previous full launcher (bridge injection loop, helper server, status
//! store, pet overlay sync) was only exercised by this crate's own tests; the
//! backend implements its own lifecycle in `backend/src/launcher`. Only the
//! command builders and platform process helpers it consumes remain here.

use std::path::Path;

#[cfg(windows)]
use anyhow::Context;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexLaunch {
    Process {
        command: Vec<String>,
        wait_strategy: ProcessWaitStrategy,
        macos_cleanup_policy: Option<MacosCleanupPolicy>,
    },
    PackagedActivation {
        app_user_model_id: String,
        arguments: String,
        process_id: Option<u32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessWaitStrategy {
    TrackedChild,
    ExternalWaitCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacosCleanupPolicy {
    QuitIfNotPreviouslyRunning,
    SkipQuitBecauseAlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsProcessControlStrategy {
    NativeWindowsApi,
}

#[cfg(windows)]
pub fn windows_process_control_strategy() -> WindowsProcessControlStrategy {
    WindowsProcessControlStrategy::NativeWindowsApi
}

impl CodexLaunch {
    pub fn process_id(&self) -> Option<u32> {
        match self {
            Self::PackagedActivation { process_id, .. } => *process_id,
            Self::Process { .. } => None,
        }
    }
}

fn normalize_codex_extra_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.trim())
        .filter(|arg| !arg.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub fn build_codex_arguments(debug_port: u16, extra_args: &[String]) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={debug_port}"),
        format!("--remote-allow-origins=http://127.0.0.1:{debug_port}"),
    ];
    args.extend(normalize_codex_extra_args(extra_args));
    args
}

pub fn build_codex_command(app_dir: &Path, debug_port: u16, extra_args: &[String]) -> Vec<String> {
    let mut command = vec![
        crate::app_paths::build_codex_executable(app_dir)
            .to_string_lossy()
            .to_string(),
    ];
    command.extend(build_codex_arguments(debug_port, extra_args));
    command
}

pub fn build_packaged_activation(
    app_dir: &Path,
    debug_port: u16,
    extra_args: &[String],
) -> Option<CodexLaunch> {
    Some(CodexLaunch::PackagedActivation {
        app_user_model_id: crate::app_paths::packaged_app_user_model_id(app_dir)?,
        arguments: command_line_arguments(&build_codex_arguments(debug_port, extra_args)),
        process_id: None,
    })
}

pub fn build_macos_open_command(
    app_dir: &Path,
    debug_port: u16,
    extra_args: &[String],
) -> Vec<String> {
    let mut command = vec![
        "open".to_string(),
        "-W".to_string(),
        "-a".to_string(),
        app_dir.to_string_lossy().to_string(),
        "--args".to_string(),
    ];
    command.extend(build_codex_arguments(debug_port, extra_args));
    command
}

#[cfg(windows)]
pub async fn wait_for_windows_process_id(process_id: u32) -> anyhow::Result<()> {
    wait_for_windows_process_exit(process_id).await.map(|_| ())
}

/// Waits for the original process handle and returns its exit code when available.
#[cfg(windows)]
pub async fn wait_for_windows_process_exit(process_id: u32) -> anyhow::Result<Option<u32>> {
    let Some(handle) = open_windows_process_for_wait(process_id)? else {
        return Ok(None);
    };
    wait_for_windows_process_handle(handle, process_id).await
}

#[cfg(windows)]
fn open_windows_process_for_wait(
    process_id: u32,
) -> anyhow::Result<Option<std::os::windows::io::OwnedHandle>> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::ERROR_INVALID_PARAMETER;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    unsafe {
        // The process may already be gone when we get here; opening a dead PID
        // fails with ERROR_INVALID_PARAMETER, which is exactly the exit we are
        // waiting for — the same rule `process_is_running` applies.
        let handle = match OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            process_id,
        ) {
            Ok(handle) => handle,
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => {
                return Ok(None);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to open Windows process id {process_id}"));
            }
        };
        // Retain this process identity even if its PID is later reused. The
        // owning handle is also released when the waiting future is cancelled.
        Ok(Some(OwnedHandle::from_raw_handle(handle.0)))
    }
}

#[cfg(windows)]
async fn wait_for_windows_process_handle(
    handle: std::os::windows::io::OwnedHandle,
    process_id: u32,
) -> anyhow::Result<Option<u32>> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

    loop {
        // A blocking INFINITE wait outlives cancellation and can prevent Tokio
        // from shutting down. Poll the original handle without blocking a worker.
        let wait_result = unsafe { WaitForSingleObject(HANDLE(handle.as_raw_handle()), 0) };
        match wait_result {
            WAIT_OBJECT_0 => {
                let mut exit_code = 0;
                // The signaled handle confirms exit independently of this
                // optional status query, including an actual exit code of 259.
                return match unsafe {
                    GetExitCodeProcess(HANDLE(handle.as_raw_handle()), &mut exit_code)
                } {
                    Ok(()) => Ok(Some(exit_code)),
                    Err(error) => {
                        let _ = crate::diagnostic_log::append_diagnostic_log(
                            "launcher.process_exit_status_failed",
                            serde_json::json!({
                                "processId": process_id,
                                "detail": error.to_string(),
                            }),
                        );
                        Ok(None)
                    }
                };
            }
            WAIT_TIMEOUT => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            WAIT_FAILED => {
                return Err(windows::core::Error::from_win32()).with_context(|| {
                    format!("failed to wait for Windows process id {process_id}")
                });
            }
            _ => anyhow::bail!(
                "unexpected wait result {} for Windows process id {process_id}",
                wait_result.0
            ),
        }
    }
}

#[cfg(not(windows))]
pub async fn wait_for_windows_process_id(process_id: u32) -> anyhow::Result<()> {
    anyhow::bail!("cannot wait for Windows process id {process_id} on this platform")
}

fn command_line_arguments(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote_windows_argument(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_windows_argument(arg: &str) -> String {
    if !arg.is_empty() && !arg.bytes().any(|byte| matches!(byte, b' ' | b'\t' | b'"')) {
        return arg.to_string();
    }
    let mut output = String::from("\"");
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                output.push_str(&"\\".repeat(backslashes * 2 + 1));
                output.push('"');
                backslashes = 0;
            }
            _ => {
                output.push_str(&"\\".repeat(backslashes));
                output.push(ch);
                backslashes = 0;
            }
        }
    }
    output.push_str(&"\\".repeat(backslashes * 2));
    output.push('"');
    output
}

#[cfg(not(windows))]
pub async fn activate_packaged_app(
    _app_user_model_id: &str,
    _arguments: &str,
) -> anyhow::Result<u32> {
    anyhow::bail!("Packaged app activation is only supported on Windows")
}

#[cfg(windows)]
pub async fn activate_packaged_app(
    app_user_model_id: &str,
    arguments: &str,
) -> anyhow::Result<u32> {
    let app_user_model_id = app_user_model_id.to_string();
    let arguments = arguments.to_string();
    tokio::task::spawn_blocking(move || {
        activate_packaged_app_blocking(&app_user_model_id, &arguments)
    })
    .await
    .context("packaged app activation task failed")?
}

#[cfg(windows)]
fn activate_packaged_app_blocking(app_user_model_id: &str, arguments: &str) -> anyhow::Result<u32> {
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::UI::Shell::{ApplicationActivationManager, IApplicationActivationManager};
    use windows::core::HSTRING;

    unsafe {
        let coinit = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let should_uninitialize = coinit.is_ok();
        coinit.ok().or_else(|error| {
            const RPC_E_CHANGED_MODE: i32 = -2147417850;
            if error.code().0 == RPC_E_CHANGED_MODE {
                Ok(())
            } else {
                Err(error)
            }
        })?;

        let result: windows::core::Result<u32> = (|| {
            let manager: IApplicationActivationManager =
                CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
            let process_id = manager.ActivateApplication(
                &HSTRING::from(app_user_model_id),
                &HSTRING::from(arguments),
                windows::Win32::UI::Shell::ACTIVATEOPTIONS(0),
            )?;
            Ok(process_id)
        })();

        if should_uninitialize {
            CoUninitialize();
        }
        result.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_arguments_drop_blank_extra_args() {
        let args = build_codex_arguments(9229, &[" ".into(), "--foo".into()]);
        assert_eq!(
            args,
            [
                "--remote-debugging-port=9229",
                "--remote-allow-origins=http://127.0.0.1:9229",
                "--foo",
            ]
        );
    }

    #[test]
    fn windows_arguments_are_quoted_for_command_lines() {
        assert_eq!(
            command_line_arguments(&["plain".into(), "has space".into(), "q\"uote".into()]),
            "plain \"has space\" \"q\\\"uote\""
        );
    }

    #[cfg(windows)]
    #[test]
    fn cancelled_process_wait_releases_handle_and_blocking_worker() {
        use std::os::windows::io::AsRawHandle;
        use std::time::Duration;
        use windows::Win32::Foundation::{GetHandleInformation, HANDLE};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (handle_closed, blocking_worker_available) = runtime.block_on(async {
            let process_id = std::process::id();
            let handle = open_windows_process_for_wait(process_id).unwrap().unwrap();
            let raw_handle = handle.as_raw_handle();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(20),
                    wait_for_windows_process_handle(handle, process_id),
                )
                .await
                .is_err()
            );
            let mut flags = 0;
            let handle_closed =
                unsafe { GetHandleInformation(HANDLE(raw_handle), &mut flags) }.is_err();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(20),
                    wait_for_windows_process_id(process_id),
                )
                .await
                .is_err()
            );
            let blocking_worker_available =
                tokio::time::timeout(Duration::from_secs(1), tokio::task::spawn_blocking(|| ()))
                    .await
                    .is_ok();
            (handle_closed, blocking_worker_available)
        });
        // Bound shutdown even if a regression leaves a blocking wait behind.
        runtime.shutdown_timeout(Duration::from_millis(100));
        assert!(handle_closed);
        assert!(blocking_worker_available);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn process_exit_completes_wait_and_releases_handle() {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        use windows::Win32::Foundation::{GetHandleInformation, HANDLE};

        for exit_code in [0, 17, 259, 0xC0000005_u32] {
            let mut child = std::process::Command::new("cmd")
                .args(["/C", "exit", &(exit_code as i32).to_string()])
                .spawn()
                .unwrap();
            let handle = child.as_handle().try_clone_to_owned().unwrap();
            let raw_handle = handle.as_raw_handle();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                wait_for_windows_process_handle(handle, child.id()),
            )
            .await;
            let mut flags = 0;
            let handle_closed =
                unsafe { GetHandleInformation(HANDLE(raw_handle), &mut flags) }.is_err();
            if result.is_err() {
                let _ = child.kill();
            }
            child.wait().unwrap();
            assert_eq!(result.unwrap().unwrap(), Some(exit_code));
            assert!(handle_closed);
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn missing_windows_process_is_already_exited() {
        wait_for_windows_process_id(u32::MAX).await.unwrap();
        assert_eq!(wait_for_windows_process_exit(u32::MAX).await.unwrap(), None);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn process_wait_requires_exit_when_status_query_is_unavailable() {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

        let wait_only_handle = |process_id| unsafe {
            let handle = OpenProcess(PROCESS_SYNCHRONIZE, false, process_id).unwrap();
            OwnedHandle::from_raw_handle(handle.0)
        };
        let current_pid = std::process::id();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                wait_for_windows_process_handle(wait_only_handle(current_pid), current_pid),
            )
            .await
            .is_err()
        );

        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit", "17"])
            .spawn()
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wait_for_windows_process_handle(wait_only_handle(child.id()), child.id()),
        )
        .await;
        if result.is_err() {
            let _ = child.kill();
        }
        child.wait().unwrap();
        assert_eq!(result.unwrap().unwrap(), None);
    }
}
