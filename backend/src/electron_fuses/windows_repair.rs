//! A narrowly scoped UAC helper. It never loads Codey configuration or writes backups.
use super::*;
use std::ffi::OsString;
use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, GetFileInformationByHandle,
    GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{PCWSTR, w};

const COMMAND: &str = "--internal-repair-codex-node-options";
const VALIDATION_FAILED: i32 = 21;
const PRIVILEGE_FAILED: i32 = 22;
const WRITE_FAILED: i32 = 23;

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn local_disk_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    let mut parts = path.components();
    matches!(parts.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        && matches!(parts.next(), Some(Component::RootDir))
        && parts.all(|part| matches!(part, Component::Normal(name) if !name.encode_wide().any(|c| c == 0 || c == b':' as u16 || c == b'"' as u16)))
}

fn validate_target(binary: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        local_disk_path(binary),
        "修复目标必须是本地磁盘上的 Codex 运行时"
    );
    let canonical = std::fs::canonicalize(binary)?;
    anyhow::ensure!(
        canonical == binary && local_disk_path(&canonical),
        "修复目标路径已变化"
    );
    let directory = canonical.parent().context("运行时缺少安装目录")?;
    let normalized = codey_runtime_core::app_paths::normalize_codex_app_path(directory)
        .context("不是有效的 Codex 安装目录")?;
    anyhow::ensure!(normalized == directory, "运行时不在 Codex 安装目录中");
    let candidate = electron_binary_path(&normalized).context("找不到 Codex 运行时")?;
    anyhow::ensure!(candidate == canonical, "目标不是 Codex 的 Electron 运行时");
    anyhow::ensure!(
        ["chrome.dll", "ChatGPT.exe", "Codex.exe"]
            .iter()
            .any(|name| canonical
                .file_name()
                .is_some_and(|actual| actual.eq_ignore_ascii_case(name))),
        "不支持的运行时文件名"
    );
    Ok(canonical)
}

fn validate_open_handle(file: &std::fs::File, binary: &Path) -> Result<()> {
    let handle = HANDLE(file.as_raw_handle());
    let mut name = vec![0u16; 32768];
    let count = unsafe { GetFinalPathNameByHandleW(handle, &mut name, VOLUME_NAME_DOS) } as usize;
    anyhow::ensure!(
        count > 0 && count < name.len(),
        "无法确认运行时句柄的最终路径"
    );
    use std::os::windows::ffi::OsStringExt;
    anyhow::ensure!(
        PathBuf::from(OsString::from_wide(&name[..count])) == binary,
        "运行时在打开期间被替换或重定向"
    );
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut information) }?;
    anyhow::ensure!(
        information.nNumberOfLinks == 1,
        "拒绝修复具有多个硬链接的运行时"
    );
    Ok(())
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn enable_privileges() -> Result<()> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }?;
    let token = HandleGuard(token);
    for name in [w!("SeBackupPrivilege"), w!("SeRestorePrivilege")] {
        let mut luid = Default::default();
        unsafe { LookupPrivilegeValueW(PCWSTR::null(), name, &mut luid) }?;
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        unsafe { AdjustTokenPrivileges(token.0, false, Some(&privileges), 0, None, None) }?;
        // A successful BOOL can still mean the requested privilege was absent.
        anyhow::ensure!(
            unsafe { GetLastError() } != ERROR_NOT_ALL_ASSIGNED,
            "管理员账户未获分配备份/还原特权"
        );
    }
    Ok(())
}

fn privileged_write(binary: &Path, expected: &str) -> std::result::Result<(), i32> {
    validate_target(binary).map_err(|_| VALIDATION_FAILED)?;
    if !valid_digest(expected) {
        return Err(VALIDATION_FAILED);
    }
    enable_privileges().map_err(|_| PRIVILEGE_FAILED)?;
    // CreateFileW backup semantics grants data access using the enabled privileges;
    // share_mode(0) keeps inspection, write, readback and rollback on one handle.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(binary)
        .map_err(|_| WRITE_FAILED)?;
    validate_open_handle(&file, binary).map_err(|_| VALIDATION_FAILED)?;
    validate_target(binary).map_err(|_| VALIDATION_FAILED)?;
    let inspection = inspect_expected(&mut file, expected).map_err(|_| VALIDATION_FAILED)?;
    write_node_options(&mut file, &inspection, b'1').map_err(|_| WRITE_FAILED)
}

fn inspect_expected(file: &mut std::fs::File, expected: &str) -> Result<NodeOptionsInspection> {
    let inspection = inspect_node_options(file)?;
    anyhow::ensure!(
        inspection.current == b'0' && inspection.digest == expected,
        "运行时与修复请求不一致"
    );
    Ok(inspection)
}

pub(crate) fn run_if_requested() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    run_helper_arguments(&args, privileged_write)
}

fn run_helper_arguments(
    args: &[OsString],
    write: impl FnOnce(&Path, &str) -> std::result::Result<(), i32>,
) -> Option<i32> {
    if args.first().and_then(|value| value.to_str()) != Some(COMMAND) {
        return None;
    }
    if args.len() != 3 {
        return Some(VALIDATION_FAILED);
    }
    let Some(digest) = args[2].to_str().filter(|digest| valid_digest(digest)) else {
        return Some(VALIDATION_FAILED);
    };
    Some(write(Path::new(&args[1]), digest).err().unwrap_or(0))
}

fn helper_exit_result(code: u32) -> Result<()> {
    match code as i32 {
        0 => Ok(()),
        VALIDATION_FAILED => {
            anyhow::bail!("管理员修复校验未通过：运行时路径、版本或内容已变化，未执行修复")
        }
        PRIVILEGE_FAILED => {
            anyhow::bail!("管理员账户未能启用备份/还原特权，无法修复受保护的运行时")
        }
        WRITE_FAILED => anyhow::bail!(
            "管理员修复写入或回读失败：运行时可能仍被占用或受系统策略保护，请保留原始字节备份"
        ),
        _ => anyhow::bail!("管理员修复进程异常退出（退出码 {code}）"),
    }
}

fn launch_error(error: windows::core::Error) -> anyhow::Error {
    if error.code() == ERROR_CANCELLED.to_hresult() {
        anyhow::anyhow!("已取消 Windows 管理员授权，未执行运行时修复")
    } else {
        anyhow::Error::new(error).context("无法启动管理员修复进程")
    }
}

fn launch_helper(binary: &Path, digest: &str) -> Result<()> {
    // Keep the OS-provided executable path in shell syntax, rather than adding
    // canonicalize's verbatim path prefix to ShellExecuteExW's lpFile.
    let executable = std::env::current_exe()?;
    let executable = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // Validated paths cannot contain quotes; a runtime filename cannot end in '\'.
    let mut parameters = OsString::from(format!("{COMMAND} \""));
    parameters.push(binary);
    parameters.push(format!("\" {digest}"));
    let parameters = parameters.encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(executable.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(launch_error)?;
    anyhow::ensure!(
        !info.hProcess.is_invalid(),
        "Windows 未返回管理员修复进程句柄"
    );
    let process = HandleGuard(info.hProcess);
    anyhow::ensure!(
        unsafe { WaitForSingleObject(process.0, INFINITE) } == WAIT_OBJECT_0,
        "等待管理员修复进程失败"
    );
    let mut code = 0;
    unsafe { GetExitCodeProcess(process.0, &mut code) }?;
    helper_exit_result(code)
}

pub(super) fn repair(binary: &Path, backup: &Path, cache: &Path) -> Result<()> {
    repair_with(binary, backup, cache, open_runtime_exclusive, launch_helper)
}

fn repair_with(
    binary: &Path,
    backup: &Path,
    cache: &Path,
    open: impl FnOnce(&Path) -> std::io::Result<std::fs::File>,
    elevate: impl FnOnce(&Path, &str) -> Result<()>,
) -> Result<()> {
    let binary = validate_target(binary)?;
    match open(&binary) {
        Ok(mut file) => {
            let inspection = inspect_node_options(&mut file)?;
            if prepare_node_options_change(&binary, backup, cache, false, &inspection)? {
                write_node_options(&mut file, &inspection, b'1')?;
            }
        }
        Err(error) if error.raw_os_error() == Some(5) => {
            let mut file = std::fs::File::open(&binary).context("无法读取待修复的运行时")?;
            validate_open_handle(&file, &binary)?;
            let inspection = inspect_node_options(&mut file)?;
            if !prepare_node_options_change(&binary, backup, cache, false, &inspection)? {
                return Ok(());
            }
            drop(file);
            elevate(&binary, &inspection.digest)?;
            let mut file = std::fs::File::open(&binary).context("管理员修复后无法读取运行时")?;
            validate_open_handle(&file, &binary)?;
            let actual = inspect_node_options(&mut file)?;
            anyhow::ensure!(
                actual.current == b'1'
                    && actual.offset == inspection.offset
                    && actual.digest == inspection.digest,
                "管理员修复后独立校验失败，未确认修复成功"
            );
        }
        Err(error) => {
            return Err(error)
                .context("无法独占写入运行时，请确认 Codex 已关闭且文件未被其他程序占用");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Codex.exe"), b"exe").unwrap();
        let binary = temp.path().join("chrome.dll");
        std::fs::write(&binary, super::super::tests::wire_bytes("010011001")).unwrap();
        let binary = binary.canonicalize().unwrap();
        let backup = temp.path().join("backup.json");
        let cache = temp.path().join("cache.json");
        (temp, binary, backup, cache)
    }
    #[test]
    fn only_target_open_access_denial_requests_elevation() {
        for code in [5, 32, 33] {
            let (_temp, binary, backup, cache) = runtime();
            let called = std::cell::Cell::new(false);
            let result = repair_with(
                &binary,
                &backup,
                &cache,
                |_| Err(std::io::Error::from_raw_os_error(code)),
                |_, _| {
                    called.set(true);
                    assert!(backup.is_file());
                    anyhow::bail!("cancelled")
                },
            );
            assert!(result.is_err());
            assert_eq!(called.get(), code == 5);
        }
    }
    #[test]
    fn parent_verifies_helper_result_and_keeps_backup_on_failure() {
        let (_temp, binary, backup, cache) = runtime();
        assert!(
            repair_with(
                &binary,
                &backup,
                &cache,
                |_| Err(std::io::Error::from_raw_os_error(5)),
                |_, _| Ok(())
            )
            .is_err()
        );
        assert!(backup.is_file());
        repair_with(
            &binary,
            &backup,
            &cache,
            |_| Err(std::io::Error::from_raw_os_error(5)),
            |path, digest| {
                let mut file = open_runtime_exclusive(path)?;
                let inspection = inspect_node_options(&mut file)?;
                assert_eq!(inspection.digest, digest);
                write_node_options(&mut file, &inspection, b'1')
            },
        )
        .unwrap();
        change_node_options(&binary, &backup, &cache, true).unwrap();
    }
    #[test]
    fn cache_failure_does_not_launch_helper() {
        let (_temp, binary, backup, cache) = runtime();
        std::fs::create_dir(&cache).unwrap();
        assert!(
            repair_with(
                &binary,
                &backup,
                &cache,
                |_| Err(std::io::Error::from_raw_os_error(5)),
                |_, _| panic!("must not elevate")
            )
            .is_err()
        );
        assert!(!backup.exists());
    }
    #[test]
    fn target_validation_rejects_network_devices_and_non_runtime_files() {
        for path in [
            r"\\server\share\chrome.dll",
            r"\\?\UNC\server\share\chrome.dll",
            r"\\.\C:\chrome.dll",
            r"C:\app\chrome.dll:stream",
            r"C:chrome.dll",
        ] {
            assert!(!local_disk_path(Path::new(path)), "{path}");
        }
        let (_temp, binary, _, _) = runtime();
        assert!(validate_target(&binary).is_ok());
        assert!(validate_target(&binary.with_file_name("Codex.exe")).is_err());
    }
    #[test]
    fn helper_command_and_exit_codes_are_limited() {
        assert_eq!(
            run_helper_arguments(&["--help".into()], |_, _| panic!()),
            None
        );
        for args in [
            vec![COMMAND.into()],
            vec![COMMAND.into(), "C:\\chrome.dll".into(), "bad".into()],
            vec![
                COMMAND.into(),
                "C:\\chrome.dll".into(),
                "a".repeat(64).into(),
                "extra".into(),
            ],
        ] {
            assert_eq!(
                run_helper_arguments(&args, |_, _| panic!()),
                Some(VALIDATION_FAILED)
            );
        }
        for code in [VALIDATION_FAILED, PRIVILEGE_FAILED, WRITE_FAILED, 99] {
            assert!(helper_exit_result(code as u32).is_err());
        }
        assert!(helper_exit_result(0).is_ok());
        assert!(
            launch_error(windows::core::Error::from_hresult(
                ERROR_CANCELLED.to_hresult()
            ))
            .to_string()
            .contains("已取消")
        );
    }
    #[test]
    fn transaction_rolls_back_after_flush_failure() {
        let (_temp, binary, _, _) = runtime();
        let original = std::fs::read(&binary).unwrap();
        let mut file = open_runtime_exclusive(&binary).unwrap();
        let inspection = inspect_node_options(&mut file).unwrap();
        let error = write_node_options_with(&mut file, &inspection, b'1', |_| {
            anyhow::bail!("flush failed")
        })
        .unwrap_err();
        assert!(error.to_string().contains("已还原"));
        drop(file);
        assert_eq!(std::fs::read(binary).unwrap(), original);
    }
    #[test]
    fn helper_rejects_stale_digest_and_already_enabled_wire() {
        let (_temp, binary, _, _) = runtime();
        let mut file = open_runtime_exclusive(&binary).unwrap();
        let inspection = inspect_node_options(&mut file).unwrap();
        assert!(inspect_expected(&mut file, &"f".repeat(64)).is_err());
        assert!(inspect_expected(&mut file, &inspection.digest).is_ok());
        write_node_options(&mut file, &inspection, b'1').unwrap();
        assert!(inspect_expected(&mut file, &inspection.digest).is_err());
    }
    #[test]
    fn helper_rejects_hardlinks_and_exclusive_handle_blocks_readers() {
        let (temp, binary, _, _) = runtime();
        let file = open_runtime_exclusive(&binary).unwrap();
        validate_open_handle(&file, &binary).unwrap();
        validate_target(&binary).unwrap();
        assert!(std::fs::File::open(&binary).is_err());
        drop(file);
        std::fs::hard_link(&binary, temp.path().join("linked.dll")).unwrap();
        let file = std::fs::File::open(&binary).unwrap();
        assert!(validate_open_handle(&file, &binary).is_err());
    }
}
