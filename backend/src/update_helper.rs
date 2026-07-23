use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const UPDATE_HELPER_FLAG: &str = "--codey-install-update";
#[cfg(target_os = "windows")]
const UPDATE_HELPER_FILE_PREFIX: &str = "install-codey-update-helper-";
#[cfg(target_os = "windows")]
const UPDATE_LOG_FILE: &str = "install-codey-update.log";
#[cfg(target_os = "windows")]
const INSTALLED_EXECUTABLE_NAME: &str = "Codey.exe";

#[derive(Clone, Debug, PartialEq, Eq)]
struct UpdateHelperInvocation {
    installer: PathBuf,
    executable: PathBuf,
    install_dir: PathBuf,
}

pub(crate) fn run_if_requested() -> Result<bool, String> {
    let Some(invocation) = parse_update_helper_invocation(std::env::args_os())? else {
        return Ok(false);
    };

    #[cfg(target_os = "windows")]
    {
        run_windows_update_helper(&invocation)?;
        Ok(true)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = invocation;
        Err("Codey 更新助手仅支持 Windows".to_string())
    }
}

fn parse_update_helper_invocation<I>(arguments: I) -> Result<Option<UpdateHelperInvocation>, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    let Some(mode) = arguments.next() else {
        return Ok(None);
    };
    if mode != OsStr::new(UPDATE_HELPER_FLAG) {
        return Ok(None);
    }

    let installer = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "更新助手缺少安装包路径".to_string())?;
    let executable = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "更新助手缺少 Codey 可执行文件路径".to_string())?;
    let install_dir = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "更新助手缺少 Codey 安装目录".to_string())?;
    if arguments.next().is_some() {
        return Err("更新助手收到多余参数".to_string());
    }

    Ok(Some(UpdateHelperInvocation {
        installer,
        executable,
        install_dir,
    }))
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn nsis_install_directory_argument(install_dir: &Path) -> OsString {
    let mut argument = OsString::from("/D=");
    argument.push(install_dir.as_os_str());
    argument
}

#[cfg(target_os = "windows")]
pub(crate) fn spawn_update_installer(update_path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    if !update_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err("Windows 更新安装包必须是 .exe".to_string());
    }

    let executable =
        std::env::current_exe().map_err(|error| format!("读取当前 Codey 路径失败：{error}"))?;
    let install_dir = executable
        .parent()
        .ok_or_else(|| "当前 Codey 路径无父目录".to_string())?;
    let update_dir = update_path
        .parent()
        .ok_or_else(|| "更新安装包路径无父目录".to_string())?;

    remove_stale_update_helpers(update_dir);
    let helper_path = update_dir.join(format!(
        "{UPDATE_HELPER_FILE_PREFIX}{}.exe",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::copy(&executable, &helper_path)
        .map_err(|error| format!("创建原生更新助手失败：{error}"))?;

    // The helper runs from the update cache so it does not keep the installed
    // Codey.exe locked while NSIS replaces it. This also avoids relying on a
    // PowerShell execution policy after the main process has already exited.
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    let spawn_result = std::process::Command::new(&helper_path)
        .arg(UPDATE_HELPER_FLAG)
        .arg(update_path)
        .arg(&executable)
        .arg(install_dir)
        .current_dir(update_dir)
        .creation_flags(
            codex_plus_core::windows_create_no_window()
                | DETACHED_PROCESS
                | CREATE_NEW_PROCESS_GROUP,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(error) = spawn_result {
        let _ = std::fs::remove_file(&helper_path);
        return Err(format!("启动原生更新助手失败：{error}"));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn remove_stale_update_helpers(update_dir: &Path) {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(update_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_helper = path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| {
                name.starts_with(UPDATE_HELPER_FILE_PREFIX)
                    && name.to_ascii_lowercase().ends_with(".exe")
            });
        let is_stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
            .is_ok_and(|age| age >= STALE_AFTER);
        if is_helper && is_stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(target_os = "windows")]
fn run_windows_update_helper(invocation: &UpdateHelperInvocation) -> Result<(), String> {
    let log_path = invocation
        .installer
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(UPDATE_LOG_FILE);
    append_update_log(
        &log_path,
        &format!(
            "Starting native Codey update. Installer={} Executable={} InstallDir={}",
            invocation.installer.display(),
            invocation.executable.display(),
            invocation.install_dir.display()
        ),
    );
    validate_windows_update_helper_invocation(invocation).inspect_err(|error| {
        append_update_log(
            &log_path,
            &format!("Update helper validation failed: {error}"),
        );
    })?;

    let install_result = install_windows_update(invocation, &log_path);
    if let Err(error) = &install_result {
        append_update_log(&log_path, &format!("Update failed: {error}"));
    }

    // Restart is deliberately attempted even after an install failure. The
    // previous implementation exited on any error and left the user with no
    // Codex window at all.
    let restart_result = restart_codey(invocation, &log_path);
    if let Err(error) = &restart_result {
        append_update_log(&log_path, &format!("Restart failed: {error}"));
    }

    match (install_result, restart_result) {
        (Ok(()), Ok(())) => {
            append_update_log(&log_path, "Update finished");
            Ok(())
        }
        (Err(install_error), Ok(())) => {
            Err(format!("更新安装失败，但已重新启动原版本：{install_error}"))
        }
        (Ok(()), Err(restart_error)) => Err(format!("更新已安装，但重新启动失败：{restart_error}")),
        (Err(install_error), Err(restart_error)) => Err(format!(
            "更新安装失败：{install_error}；重新启动也失败：{restart_error}"
        )),
    }
}

#[cfg(target_os = "windows")]
fn validate_windows_update_helper_invocation(
    invocation: &UpdateHelperInvocation,
) -> Result<(), String> {
    let helper = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("读取更新助手路径失败：{error}"))?;
    let helper_name = helper
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "更新助手文件名无效".to_string())?;
    if !helper_name.starts_with(UPDATE_HELPER_FILE_PREFIX)
        || !helper_name.to_ascii_lowercase().ends_with(".exe")
    {
        return Err("更新助手必须从 Codey 更新缓存副本运行".to_string());
    }
    let helper_dir = helper
        .parent()
        .ok_or_else(|| "更新助手路径无父目录".to_string())?;

    let installer = invocation
        .installer
        .canonicalize()
        .map_err(|error| format!("读取更新安装包失败：{error}"))?;
    if !installer
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err("Windows 更新安装包必须是 .exe".to_string());
    }
    if installer.parent() != Some(helper_dir) {
        return Err("更新安装包和更新助手必须位于同一个 Codey 更新缓存目录".to_string());
    }

    let executable = invocation
        .executable
        .canonicalize()
        .map_err(|error| format!("读取 Codey 可执行文件失败：{error}"))?;
    let install_dir = invocation
        .install_dir
        .canonicalize()
        .map_err(|error| format!("读取 Codey 安装目录失败：{error}"))?;
    if executable.parent() != Some(install_dir.as_path()) {
        return Err("Codey 可执行文件不在指定安装目录中".to_string());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_windows_update(
    invocation: &UpdateHelperInvocation,
    log_path: &Path,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    wait_for_executable_unlock(&invocation.executable, std::time::Duration::from_secs(180))?;
    append_update_log(log_path, "Installed executable lock released");

    let mut command = std::process::Command::new(&invocation.installer);
    command
        .arg("/S")
        // NSIS requires /D=... to be the final raw argument. In particular,
        // paths containing spaces must not be wrapped in quotes.
        .raw_arg(nsis_install_directory_argument(&invocation.install_dir))
        .current_dir(
            invocation
                .installer
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    append_update_log(log_path, "Running NSIS installer");
    let status = command
        .status()
        .map_err(|error| format!("启动更新安装包失败：{error}"))?;
    append_update_log(log_path, &format!("Installer exited with status {status}"));
    if !status.success() {
        return Err(format!("安装包返回失败状态：{status}"));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn wait_for_executable_unlock(path: &Path, timeout: std::time::Duration) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;

    if !path.exists() {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let error = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
        {
            Ok(file) => {
                drop(file);
                return Ok(());
            }
            Err(error) => error,
        };
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "等待 Codey 可执行文件释放超时（{}）：{error}",
                path.display(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(target_os = "windows")]
fn restart_codey(invocation: &UpdateHelperInvocation, log_path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    let installed_executable = invocation.install_dir.join(INSTALLED_EXECUTABLE_NAME);
    let mut candidates = vec![installed_executable];
    if !candidates
        .iter()
        .any(|candidate| candidate == &invocation.executable)
    {
        candidates.push(invocation.executable.clone());
    }

    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    let mut failures = Vec::new();
    for target in candidates {
        if !target.is_file() {
            failures.push(format!("{} 不存在", target.display()));
            continue;
        }
        append_update_log(log_path, &format!("Restarting Codey: {}", target.display()));
        let current_dir = target.parent().unwrap_or_else(|| Path::new("."));
        match std::process::Command::new(&target)
            .current_dir(current_dir)
            .creation_flags(
                codex_plus_core::windows_create_no_window()
                    | DETACHED_PROCESS
                    | CREATE_NEW_PROCESS_GROUP,
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(error) => failures.push(format!("{}：{error}", target.display())),
        }
    }
    Err(failures.join("；"))
}

#[cfg(target_os = "windows")]
fn append_update_log(log_path: &Path, message: &str) {
    use std::io::Write;

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        let _ = writeln!(log, "[{timestamp}] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_startup_does_not_enter_update_helper_mode() {
        let parsed = parse_update_helper_invocation([
            OsString::from("Codey.exe"),
            OsString::from("--some-normal-argument"),
        ])
        .unwrap();

        assert_eq!(parsed, None);
    }

    #[test]
    fn update_helper_arguments_preserve_paths_with_spaces() {
        let parsed = parse_update_helper_invocation([
            OsString::from("helper.exe"),
            OsString::from(UPDATE_HELPER_FLAG),
            OsString::from(r"C:\Users\Test User\updates\Codey setup.exe"),
            OsString::from(r"C:\Users\Test User\Programs\Codey\Codey.exe"),
            OsString::from(r"C:\Users\Test User\Programs\Codey"),
        ])
        .unwrap()
        .unwrap();

        assert_eq!(
            parsed,
            UpdateHelperInvocation {
                installer: PathBuf::from(r"C:\Users\Test User\updates\Codey setup.exe"),
                executable: PathBuf::from(r"C:\Users\Test User\Programs\Codey\Codey.exe"),
                install_dir: PathBuf::from(r"C:\Users\Test User\Programs\Codey"),
            }
        );
        assert_eq!(
            nsis_install_directory_argument(&parsed.install_dir),
            OsString::from(r"/D=C:\Users\Test User\Programs\Codey")
        );
    }

    #[test]
    fn malformed_update_helper_invocation_is_rejected() {
        let error = parse_update_helper_invocation([
            OsString::from("helper.exe"),
            OsString::from(UPDATE_HELPER_FLAG),
            OsString::from("installer.exe"),
        ])
        .unwrap_err();

        assert!(error.contains("Codey 可执行文件路径"));
    }
}
