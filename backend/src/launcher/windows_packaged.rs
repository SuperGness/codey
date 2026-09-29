//! Per-process environment delivery for registered Store desktop executables.
//! No package files, Electron fuses or persistent debugger settings are changed.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Globalization::{CSTR_EQUAL, CSTR_LESS_THAN, CompareStringOrdinal};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    PROCESS_INFORMATION, PROCESS_NAME_WIN32, QueryFullProcessImageNameW, ResumeThread,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use super::platform::{
    WindowsPackageChanged, WindowsStartupProcess, normalized_windows_path,
    startup_activation_error_after_cleanup,
};
use super::{SpawnedCodex, recovery};

fn wide(value: &OsStr) -> Result<Vec<u16>> {
    let value: Vec<_> = value.encode_wide().collect();
    anyhow::ensure!(!value.contains(&0), "Windows 启动参数或环境包含空字符");
    anyhow::ensure!(
        value.len() < i32::MAX as usize,
        "Windows 启动参数或环境过长"
    );
    Ok(value)
}

fn compare_names(left: &[u16], right: &[u16]) -> Ordering {
    // Both slices have been length-checked. Use Windows ordinal case folding,
    // preserving non-UTF-8 environment values and hidden drive-directory keys.
    match unsafe { CompareStringOrdinal(left, right, true) } {
        CSTR_EQUAL => Ordering::Equal,
        CSTR_LESS_THAN => Ordering::Less,
        _ => Ordering::Greater,
    }
}

fn environment_block(command: &Command) -> Result<Vec<u16>> {
    // Callers use inherited environments with explicit overrides/removals;
    // they never use Command::env_clear (which get_envs does not expose).
    let mut entries = std::env::vars_os()
        .map(|(key, value)| Ok((wide(&key)?, wide(&value)?)))
        .collect::<Result<Vec<_>>>()?;
    for (name, value) in command.get_envs() {
        let name = wide(name)?;
        anyhow::ensure!(
            !name.is_empty() && !name.contains(&(b'=' as u16)),
            "Windows 启动环境变量名称无效"
        );
        entries.retain(|(key, _)| compare_names(key, &name) != Ordering::Equal);
        if let Some(value) = value {
            entries.push((name, wide(value)?));
        }
    }
    entries.sort_by(|(left, _), (right, _)| compare_names(left, right));
    entries.dedup_by(|(left, _), (right, _)| compare_names(left, right) == Ordering::Equal);
    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(name);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn command_line(command: &Command) -> Result<Vec<u16>> {
    let mut line = Vec::new();
    // Always quote each argument using Windows CRT backslash/quote rules.
    // Work in UTF-16 to preserve paths that are not representable as UTF-8.
    for value in std::iter::once(command.get_program()).chain(command.get_args()) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        line.push(b'"' as u16);
        let mut slashes = 0;
        for unit in wide(value)? {
            if unit == 0x5c {
                slashes += 1;
                continue;
            }
            let count = if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            };
            line.extend(std::iter::repeat_n(0x5c, count));
            slashes = 0;
            line.push(unit);
        }
        line.extend(std::iter::repeat_n(0x5c, slashes * 2));
        line.push(b'"' as u16);
    }
    line.push(0);
    anyhow::ensure!(line.len() <= 32767, "Windows 启动命令过长");
    Ok(line)
}

struct SuspendedProcess {
    process: Option<WindowsStartupProcess>,
    thread: OwnedHandle,
    process_id: u32,
}

impl SuspendedProcess {
    fn create(command: &Command) -> Result<Self> {
        let mut executable = wide(command.get_program())?;
        executable.push(0);
        let mut arguments = command_line(command)?;
        let environment = environment_block(command)?;
        let directory = command
            .get_current_dir()
            .map(|path| {
                let mut value = wide(path.as_os_str())?;
                value.push(0);
                Ok::<_, anyhow::Error>(value)
            })
            .transpose()?;
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR(executable.as_ptr()),
                PWSTR(arguments.as_mut_ptr()),
                None,
                None,
                false,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                Some(environment.as_ptr().cast()),
                directory
                    .as_ref()
                    .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr())),
                &startup,
                &mut info,
            )
        }
        .context("创建携带运行环境的 Windows Store Codex 进程失败")?;
        // Own both exact handles immediately; never rediscover the primary
        // thread by PID or resume another process returned by single instance.
        Ok(Self {
            process: Some(WindowsStartupProcess(unsafe {
                OwnedHandle::from_raw_handle(info.hProcess.0)
            })),
            thread: unsafe { OwnedHandle::from_raw_handle(info.hThread.0) },
            process_id: info.dwProcessId,
        })
    }

    fn verify(&self, expected_package: &str, executable: &Path) -> Result<()> {
        let process = self
            .process
            .as_ref()
            .context("Windows 启动进程句柄已释放")?;
        let actual_package = process
            .package_full_name()
            .context("Windows 未向新进程授予已注册包身份，已取消集成启动")?;
        if actual_package != expected_package {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_package_changed_during_activation",
                serde_json::json!({"expectedPackage": expected_package, "actualPackage": actual_package, "processId": self.process_id}),
            );
            return Err(WindowsPackageChanged.into());
        }
        let mut image = vec![0u16; 32768];
        let mut length = image.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                HANDLE(process.0.as_raw_handle()),
                PROCESS_NAME_WIN32,
                PWSTR(image.as_mut_ptr()),
                &mut length,
            )
        }
        .context("读取 Windows Store Codex 进程路径失败")?;
        let image = OsString::from_wide(&image[..length as usize]);
        let image = std::fs::canonicalize(Path::new(&image))
            .context("核验 Windows Store Codex 进程路径失败")?;
        anyhow::ensure!(
            normalized_windows_path(&image) == normalized_windows_path(executable),
            "Windows Store Codex 实际启动程序与已验证安装不一致"
        );
        Ok(())
    }

    fn resume(&self) -> Result<()> {
        let previous = unsafe { ResumeThread(HANDLE(self.thread.as_raw_handle())) };
        if previous == u32::MAX {
            return Err(windows::core::Error::from_win32())
                .context("恢复 Windows Store Codex 主线程失败");
        }
        anyhow::ensure!(previous == 1, "Windows Store Codex 主线程暂停状态异常");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(process) = &self.process {
            let handle = HANDLE(process.0.as_raw_handle());
            if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
                let terminated = unsafe { TerminateProcess(handle, 1) };
                // A racing exit can make TerminateProcess fail. Only a signaled
                // handle proves cleanup; a timeout must prohibit native recovery.
                if unsafe { WaitForSingleObject(handle, 8000) } != WAIT_OBJECT_0 {
                    terminated.context("停止未通过校验的 Windows Codex 进程失败")?;
                    anyhow::bail!("等待未通过校验的 Windows Codex 进程退出失败");
                }
            }
        }
        self.process.take();
        Ok(())
    }
}

impl Drop for SuspendedProcess {
    fn drop(&mut self) {
        // Also cover early returns and unwinding before the process is handed
        // to the normal startup monitor. Explicit failure paths confirm cleanup.
        if let Err(error) = self.stop() {
            crate::error_log::record_failure(
                "cleanup_failed",
                "stop_suspended_windows_codex",
                format!("{error:#}"),
                serde_json::json!({"processId": self.process_id}),
            );
        }
    }
}

pub(super) fn spawn_with_environment(app_dir: &Path, command: &Command) -> Result<SpawnedCodex> {
    let expected_package = codey_runtime_core::app_paths::packaged_app_full_name(app_dir)
        .context("无法确认 Windows Store Codex 包身份")?;
    let executable = std::fs::canonicalize(codey_runtime_core::app_paths::build_codex_executable(
        app_dir,
    ))
    .context("无法确认 Windows Store Codex 启动程序")?;
    anyhow::ensure!(
        std::fs::canonicalize(command.get_program())? == executable,
        "Windows Store Codex 启动命令与所选安装不一致"
    );
    let mut pending = SuspendedProcess::create(command).map_err(recovery::recoverable)?;
    if let Err(error) = pending
        .verify(&expected_package, &executable)
        .and_then(|_| pending.resume())
    {
        let stopped = pending.stop();
        let error = if error.is::<WindowsPackageChanged>() {
            error
        } else {
            recovery::recoverable(error)
        };
        return Err(startup_activation_error_after_cleanup(
            error,
            stopped,
            Ok(()),
        ));
    }
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.windows_package_environment_started",
        serde_json::json!({"processId": pending.process_id, "package": expected_package, "environmentApplied": true}),
    );
    Ok(SpawnedCodex {
        child: None,
        process_id: Some(pending.process_id),
        startup_process: pending.process.take(),
        performance_status: String::new(),
        performance_detail: String::new(),
        startup_injection_mode: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_OUTPUT: &str = "CODEY_TEST_WINDOWS_LAUNCH_PROBE";
    const PROBE_VALUE: &str = "CODEY_TEST_WINDOWS_LAUNCH_VALUE";

    fn probe_command(output: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "launcher::windows_packaged::tests::launch_probe_child",
            "--nocapture",
        ]);
        command.env(PROBE_OUTPUT, output);
        command.env(PROBE_VALUE, "带空格的 配置 = value");
        command.env_remove("PATH");
        command
    }

    #[test]
    fn launch_probe_child() {
        let Some(output) = std::env::var_os(PROBE_OUTPUT) else {
            return;
        };
        let data = serde_json::json!({
            "value": std::env::var(PROBE_VALUE).unwrap(),
            "pathRemoved": std::env::var_os("PATH").is_none(),
        });
        std::fs::write(output, data.to_string()).unwrap();
    }

    #[test]
    fn suspended_child_receives_environment_only_after_resume() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("probe.json");
        let mut pending = SuspendedProcess::create(&probe_command(&output)).unwrap();
        assert!(!output.exists());
        assert_eq!(pending.process.as_ref().unwrap().exit_code().unwrap(), None);
        pending.resume().unwrap();
        let handle = HANDLE(pending.process.as_ref().unwrap().0.as_raw_handle());
        assert_eq!(unsafe { WaitForSingleObject(handle, 10000) }, WAIT_OBJECT_0);
        assert_eq!(
            pending.process.as_ref().unwrap().exit_code().unwrap(),
            Some(0)
        );
        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(data["value"], "带空格的 配置 = value");
        assert_eq!(data["pathRemoved"], true);
        pending.stop().unwrap();
    }

    #[test]
    fn rejected_package_is_stopped_without_executing_its_entrypoint() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("must-not-exist");
        let mut pending = SuspendedProcess::create(&probe_command(&output)).unwrap();
        let observer = WindowsStartupProcess::open(pending.process_id).unwrap();
        assert!(
            pending
                .verify(
                    "OpenAI.Codex_0.0.0.0_x64__2p2nqsd0c76g0",
                    &std::env::current_exe().unwrap()
                )
                .is_err()
        );
        pending.stop().unwrap();
        assert!(observer.exit_code().unwrap().is_some());
        assert!(!output.exists());
    }

    #[test]
    fn abandoned_suspended_child_is_terminated_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("must-not-exist");
        let pending = SuspendedProcess::create(&probe_command(&output)).unwrap();
        let observer = WindowsStartupProcess::open(pending.process_id).unwrap();
        drop(pending);
        assert!(observer.exit_code().unwrap().is_some());
        assert!(!output.exists());
    }

    #[test]
    fn arguments_round_trip_through_the_windows_parser() {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::UI::Shell::CommandLineToArgvW;

        let mut command = Command::new("C:/Program Files/客户端/Codex.exe");
        command.args(["", "space value", "--flag=value"]);
        command.arg(OsString::from_wide(&[0x5c, 0x5c, 0x22, 0x5c]));
        command.arg(OsString::from_wide(&[0x61, 0xd800, 0x62]));
        let expected = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(OsStr::to_os_string)
            .collect::<Vec<_>>();
        let encoded = command_line(&command).unwrap();
        unsafe {
            let mut count = 0;
            let args = CommandLineToArgvW(PCWSTR(encoded.as_ptr()), &mut count);
            assert!(!args.is_null());
            let actual = std::slice::from_raw_parts(args, count as usize)
                .iter()
                .map(|value| {
                    let mut length = 0;
                    while *value.0.add(length) != 0 {
                        length += 1;
                    }
                    OsString::from_wide(std::slice::from_raw_parts(value.0, length))
                })
                .collect::<Vec<_>>();
            let _ = LocalFree(HLOCAL(args.cast()));
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn environment_overrides_are_case_insensitive_and_preserve_utf16() {
        let mut command = Command::new("unused.exe");
        command
            .env("pAtH", "replacement 路径")
            .env_remove("SystemRoot");
        let unusual = OsString::from_wide(&[0x61, 0xd800, 0x62]);
        command.env(PROBE_VALUE, &unusual);
        let block = environment_block(&command).unwrap();
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        let entries = block
            .split(|unit| *unit == 0)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let separator = entry[1..]
                    .iter()
                    .position(|unit| *unit == b'=' as u16)
                    .unwrap()
                    + 1;
                (&entry[..separator], &entry[separator + 1..])
            })
            .collect::<Vec<_>>();
        let path = wide(OsStr::new("PATH")).unwrap();
        let matching = entries
            .iter()
            .filter(|(key, _)| compare_names(key, &path) == Ordering::Equal)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].1, wide(OsStr::new("replacement 路径")).unwrap());
        assert!(!entries.iter().any(|(key, _)| compare_names(
            key,
            &wide(OsStr::new("SystemRoot")).unwrap()
        ) == Ordering::Equal));
        let value_key = wide(OsStr::new(PROBE_VALUE)).unwrap();
        assert_eq!(
            entries.iter().find(|(key, _)| *key == value_key).unwrap().1,
            wide(&unusual).unwrap()
        );
        assert!(
            entries
                .windows(2)
                .all(|pair| compare_names(pair[0].0, pair[1].0) == Ordering::Less)
        );
    }

    #[test]
    fn embedded_nul_is_rejected_before_process_creation() {
        let mut command = Command::new("unused.exe");
        command.arg(OsString::from_wide(&[0x61, 0, 0x62]));
        assert!(command_line(&command).is_err());
        let mut command = Command::new("unused.exe");
        command.env(PROBE_VALUE, OsString::from_wide(&[0x61, 0, 0x62]));
        assert!(environment_block(&command).is_err());
    }
}
