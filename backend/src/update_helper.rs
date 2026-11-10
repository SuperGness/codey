use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg(target_os = "windows")]
use sha2::{Digest, Sha256};

const UPDATE_HELPER_FLAG: &str = "--codey-install-update";
#[cfg(target_os = "windows")]
const UPDATE_HELPER_FILE_PREFIX: &str = "install-codey-update-helper-";
#[cfg(target_os = "windows")]
const UPDATE_LOG_FILE: &str = "install-codey-update.log";
#[cfg(target_os = "windows")]
const INSTALLED_EXECUTABLE_NAME: &str = "Codey.exe";
/// NSIS 安装器写入安装目录的版本标记。助手据此确认安装真的落到了目标目录，
/// 避免"安装器退出码为 0 但文件没有替换"被当成成功。
#[cfg(target_os = "windows")]
const INSTALLED_VERSION_FILE: &str = "version.txt";

/// 更新助手把安装结果写给下一次启动的 Codey。文件位于配置目录，跨重启存活，
/// 由控制台读取一次后删除。
pub(crate) const UPDATE_INSTALL_REPORT_FILE: &str = "update-install-report.json";
const UPDATE_INSTALL_REPORT_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
struct UpdateHelperInvocation {
    installer: PathBuf,
    executable: PathBuf,
    install_dir: PathBuf,
    expected_size: u64,
    expected_sha256: String,
    /// 主进程显式给出的结果报告路径。助手自己按 `canonicalize` 后的安装包路径
    /// 反推配置目录会受 `\\?\` 前缀影响，落在错误的目录里，因此这条路必须由
    /// 调用方指定。
    report_path: Option<PathBuf>,
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateInstallOutcome {
    Updated,
    Failed,
    /// 安装结果无法判定：既没有版本标记，也没有观察到文件被替换。旧版本安装
    /// 目录没有版本标记时可能出现，此时不能声称更新成功。
    Unverified,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateInstallReport {
    pub(crate) version: String,
    /// 助手启动后立即写入；真正结束后改写。控制台据此区分"助手已接手"和
    /// "助手在中途消失"。
    pub(crate) status: String,
    pub(crate) message: String,
    pub(crate) written_at: u64,
}

impl UpdateInstallReport {
    pub(crate) fn is_stale(&self, now: u64) -> bool {
        now.saturating_sub(self.written_at) > UPDATE_INSTALL_REPORT_STALE_AFTER.as_secs()
    }
}

pub(crate) fn update_install_report_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join(UPDATE_INSTALL_REPORT_FILE))
        .unwrap_or_else(|| PathBuf::from(UPDATE_INSTALL_REPORT_FILE))
}

#[cfg(test)]
pub(crate) fn write_update_install_report(config_path: &Path, report: &UpdateInstallReport) {
    write_update_install_report_at(&update_install_report_path(config_path), report);
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
pub(crate) fn write_update_install_report_at(path: &Path, report: &UpdateInstallReport) {
    let Ok(encoded) = serde_json::to_vec(report) else {
        return;
    };
    let temporary = path.with_extension("json.writing");
    if std::fs::write(&temporary, encoded).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

pub(crate) fn read_update_install_report(config_path: &Path) -> Option<UpdateInstallReport> {
    read_update_install_report_at(&update_install_report_path(config_path))
}

pub(crate) fn read_update_install_report_at(path: &Path) -> Option<UpdateInstallReport> {
    let body = std::fs::read(path).ok()?;
    serde_json::from_slice(&body).ok()
}

#[cfg(target_os = "windows")]
pub(crate) fn update_install_report_exists(config_path: &Path) -> bool {
    update_install_report_path(config_path).is_file()
}

pub(crate) fn clear_update_install_report(config_path: &Path) {
    clear_update_install_report_at(&update_install_report_path(config_path));
}

pub(crate) fn clear_update_install_report_at(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("json.writing"));
}

fn unix_timestamp(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
pub(crate) fn current_unix_timestamp() -> u64 {
    unix_timestamp(SystemTime::now())
}

/// 解析安装目录里的版本标记。文件内容可能是纯版本号，也可能带说明前缀，
/// 因此只取第一个像版本号的候选。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
pub(crate) fn parse_installed_version_marker(contents: &str) -> Option<String> {
    contents
        .split_whitespace()
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            let mut parts = token.split('.');
            let segments = parts.by_ref().take(3).collect::<Vec<_>>();
            segments.len() == 3
                && segments.iter().all(|segment| {
                    !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit())
                })
        })
        .map(ToString::to_string)
}

#[cfg(target_os = "windows")]
fn read_installed_version_marker(install_dir: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(install_dir.join(INSTALLED_VERSION_FILE)).ok()?;
    parse_installed_version_marker(&contents)
}

/// 安装前记录目标可执行文件的指纹，用于判断安装器是否真的替换了文件。
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug)]
struct ExecutableFingerprint {
    size: u64,
    modified: Option<SystemTime>,
}

#[cfg(target_os = "windows")]
fn executable_fingerprint(path: &Path) -> Option<ExecutableFingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(ExecutableFingerprint {
        size: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

#[cfg(target_os = "windows")]
fn executable_was_replaced(path: &Path, before: Option<ExecutableFingerprint>) -> bool {
    let Some(before) = before else {
        return path.is_file();
    };
    let Some(after) = executable_fingerprint(path) else {
        return false;
    };
    if after.size != before.size {
        return true;
    }
    match (before.modified, after.modified) {
        (Some(before), Some(after)) => after > before,
        // 文件系统不提供修改时间时只能按大小判断，无法证明替换过。
        _ => false,
    }
}

/// 从安装包文件名解析版本号：`Codey-<version>-windows-x64-setup.exe`、
/// 旧版的 `Codey setup <version>.exe`、以及带 v 前缀的写法都接受。只接受
/// 恰好三段数字，避免把 `x64` 之类的标记当成版本。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
pub(crate) fn version_from_installer_name(file_name: &str) -> Option<String> {
    let stem = file_name
        .strip_suffix(".exe")
        .or_else(|| file_name.strip_suffix(".EXE"))
        .unwrap_or(file_name);
    let bytes = stem.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let starts_version = bytes[index].is_ascii_digit()
            && (index == 0 || !bytes[index - 1].is_ascii_digit() || bytes[index - 1] == b'.');
        if !starts_version {
            index += 1;
            continue;
        }
        let start = index;
        let mut segments: Vec<&str> = Vec::new();
        let mut cursor = index;
        loop {
            let digits_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if cursor == digits_start {
                break;
            }
            segments.push(&stem[digits_start..cursor]);
            if cursor < bytes.len() && bytes[cursor] == b'.' {
                cursor += 1;
                continue;
            }
            break;
        }
        let ends_cleanly = cursor >= bytes.len() || !bytes[cursor].is_ascii_digit();
        if segments.len() == 3 && ends_cleanly {
            return Some(segments.join("."));
        }
        index = start + 1;
    }
    None
}

/// 安装结果判定策略（与平台无关，便于直接测试）：
/// - 期望版本已知时，安装目录的版本标记必须一致，且可执行文件必须真的换过；
/// - 期望版本未知（文件名解析不出，例如预发布号）时只能依赖文件指纹；
/// - 没有版本标记（旧版本装出来的目录）时不能谎报成功，无法证明的结果按
///   未验证处理，让重启后的新版自行确认。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
pub(crate) fn decide_install_outcome(
    installed_marker: Option<&str>,
    expected_version: Option<&str>,
    executable_replaced: bool,
    executable_exists: bool,
) -> UpdateInstallOutcome {
    match (installed_marker, expected_version) {
        (Some(installed), Some(expected)) => {
            if installed != expected || (executable_replaced && !executable_exists) {
                UpdateInstallOutcome::Failed
            } else if executable_replaced {
                UpdateInstallOutcome::Updated
            } else {
                // 标记写对了但没看到文件被换过，不能断言成功，也不必判死。
                UpdateInstallOutcome::Unverified
            }
        }
        _ => {
            // 文件在且换了，或文件从来不存在而这次被装出来，都算成功。
            if executable_replaced || (!executable_exists && installed_marker.is_some()) {
                UpdateInstallOutcome::Updated
            } else {
                UpdateInstallOutcome::Unverified
            }
        }
    }
}

/// 安装结果判定：以安装目录的版本标记为准，辅以可执行文件指纹。
#[cfg(target_os = "windows")]
fn verify_installed_update(
    invocation: &UpdateHelperInvocation,
    expected_version: Option<&str>,
    before: Option<ExecutableFingerprint>,
) -> UpdateInstallOutcome {
    let installed_executable = invocation.install_dir.join(INSTALLED_EXECUTABLE_NAME);
    let marker = read_installed_version_marker(&invocation.install_dir);
    decide_install_outcome(
        marker.as_deref(),
        expected_version,
        executable_was_replaced(&installed_executable, before),
        installed_executable.is_file(),
    )
}

/// 报告路径优先取主进程显式给出的参数；旧版本主进程没有这个参数时，退回按
/// 更新缓存位置推导：`<配置目录>/updates/v<版本>/` 的祖父目录就是配置目录。
#[cfg(target_os = "windows")]
fn report_path_for_invocation(invocation: &UpdateHelperInvocation) -> Option<PathBuf> {
    if let Some(report_path) = &invocation.report_path {
        return Some(report_path.clone());
    }
    let version_dir = invocation.installer.parent()?;
    let update_root = version_dir.parent()?;
    let config_dir = update_root.parent()?;
    Some(config_dir.join(UPDATE_INSTALL_REPORT_FILE))
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

/// 主进程在退出前等待助手写下的启动记录，确认"安装流程已经被接手"再退出。
/// 助手可能在真正开始前就失败（例如自身被安全软件拦下），此时主进程保持
/// 运行并把错误交给前端展示，而不是让用户面对一个没有后续的空窗。
#[cfg(target_os = "windows")]
pub(crate) fn wait_for_helper_start(config_path: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if update_install_report_exists(config_path) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
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
    let expected_size = arguments
        .next()
        .ok_or_else(|| "更新助手缺少安装包大小".to_string())?
        .to_str()
        .ok_or_else(|| "更新助手收到无效的安装包大小".to_string())?
        .parse::<u64>()
        .map_err(|_| "更新助手收到无效的安装包大小".to_string())?;
    if expected_size == 0 {
        return Err("更新助手收到无效的安装包大小".to_string());
    }
    let expected_sha256 = arguments
        .next()
        .ok_or_else(|| "更新助手缺少安装包 SHA-256".to_string())?
        .into_string()
        .map_err(|_| "更新助手收到无效的安装包 SHA-256".to_string())?;
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("更新助手收到无效的安装包 SHA-256".to_string());
    }
    // 报告路径是可选的第 6 个参数；旧版本主进程启动的助手没有它，退回按更新
    // 缓存位置推导。
    let report_path = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        return Err("更新助手收到多余参数".to_string());
    }

    Ok(Some(UpdateHelperInvocation {
        installer,
        executable,
        install_dir,
        expected_size,
        expected_sha256,
        report_path,
    }))
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn nsis_install_directory_argument(install_dir: &Path) -> OsString {
    let mut argument = OsString::from("/D=");
    argument.push(install_dir.as_os_str());
    argument
}

/// 安装包位于 `<配置目录>/updates/v<版本>/`。主进程在配置目录等待回执，
/// 少退一层会把文件写进 `updates`，等待方永远看不到助手已启动。
#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn update_helper_report_path(update_path: &Path) -> Option<PathBuf> {
    let version_dir = update_path.parent()?;
    let updates_dir = version_dir.parent()?;
    let config_dir = updates_dir.parent()?;
    Some(config_dir.join(UPDATE_INSTALL_REPORT_FILE))
}

#[cfg(target_os = "windows")]
pub(crate) fn spawn_update_installer(
    update_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), String> {
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

    cleanup_previous_update_helpers(update_dir);
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
    // 回执必须和主进程等待的位置相同：配置目录，而不是 updates 这一层。
    let report_path = update_helper_report_path(update_path);
    let mut command = std::process::Command::new(&helper_path);
    command
        .arg(UPDATE_HELPER_FLAG)
        .arg(update_path)
        .arg(&executable)
        .arg(install_dir)
        .arg(expected_size.to_string())
        .arg(expected_sha256);
    if let Some(report_path) = &report_path {
        command.arg(report_path);
    }
    let spawn_result = command
        .current_dir(update_dir)
        .creation_flags(
            codey_runtime_core::windows_create_no_window()
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
fn run_windows_update_helper(invocation: &UpdateHelperInvocation) -> Result<(), String> {
    let log_path = invocation
        .installer
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(UPDATE_LOG_FILE);
    if let Some(update_dir) = invocation.installer.parent() {
        cleanup_previous_update_helpers(update_dir);
    }
    append_update_log(
        &log_path,
        &format!(
            "Starting native Codey update. Installer={} Executable={} InstallDir={}",
            invocation.installer.display(),
            invocation.executable.display(),
            invocation.install_dir.display()
        ),
    );

    let expected_version = invocation
        .installer
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(version_from_installer_name);
    let report_version = expected_version.clone().unwrap_or_default();
    let report_path = report_path_for_invocation(invocation);

    // 先落一份"已接手"记录：主进程据此确认可以安全退出，用户也可能在安装
    // 失败后重新打开 Codey 看到这条状态。
    if let Some(path) = &report_path {
        write_update_install_report_at(
            path,
            &UpdateInstallReport {
                version: report_version.clone(),
                status: "started".to_string(),
                message: "正在安装更新".to_string(),
                written_at: current_unix_timestamp(),
            },
        );
    }

    if let Err(error) = validate_windows_update_helper_invocation(invocation) {
        append_update_log(
            &log_path,
            &format!("Update helper validation failed: {error}"),
        );
        finish_update_report(&report_path, &report_version, "failed", &error);
        return Err(error);
    }

    let outcome = install_windows_update(invocation, &log_path);

    match outcome {
        Ok(UpdateInstallOutcome::Updated | UpdateInstallOutcome::Failed) => {
            match restart_codey(invocation, &log_path) {
                Ok(()) => {
                    append_update_log(&log_path, "Update finished");
                    finish_update_report(&report_path, &report_version, "installed", "");
                    Ok(())
                }
                Err(restart_error) => {
                    append_update_log(&log_path, &format!("Restart failed: {restart_error}"));
                    let message = format!("更新已安装，但重新启动失败：{restart_error}");
                    finish_update_report(&report_path, &report_version, "failed", &message);
                    Err(message)
                }
            }
        }
        // 安装结果无法证实（通常是从没有版本标记的老目录升级）。此时文件已经
        // 换过，交给重启后的新版自行确认，不向用户报错。
        Ok(UpdateInstallOutcome::Unverified) => {
            append_update_log(
                &log_path,
                "Install result unverified; restarting to confirm",
            );
            match restart_codey(invocation, &log_path) {
                Ok(()) => {
                    finish_update_report(
                        &report_path,
                        &report_version,
                        "unverified",
                        "无法完全确认更新结果，请核对版本号",
                    );
                    Ok(())
                }
                Err(restart_error) => {
                    let message = format!("更新结果无法确认，且重新启动失败：{restart_error}");
                    append_update_log(&log_path, &format!("Restart failed: {restart_error}"));
                    finish_update_report(&report_path, &report_version, "failed", &message);
                    Err(message)
                }
            }
        }
        // 安装明确没有落地时不再重启：旧版本会再次检测到同一个更新，用户会
        // 陷入"重启还是旧版本"的循环。把失败原因留给下一次启动的 Codey 展示，
        // 让用户看到结果并自行重试。
        Err(install_error) => {
            append_update_log(&log_path, &format!("Update failed: {install_error}"));
            finish_update_report(&report_path, &report_version, "failed", &install_error);
            Err(install_error)
        }
    }
}

#[cfg(target_os = "windows")]
fn finish_update_report(report_path: &Option<PathBuf>, version: &str, status: &str, message: &str) {
    let Some(path) = report_path else {
        return;
    };
    write_update_install_report_at(
        path,
        &UpdateInstallReport {
            version: version.to_string(),
            status: status.to_string(),
            message: message.to_string(),
            written_at: current_unix_timestamp(),
        },
    );
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
) -> Result<UpdateInstallOutcome, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    let installed_executable = invocation.install_dir.join(INSTALLED_EXECUTABLE_NAME);
    let expected_version = invocation
        .installer
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(version_from_installer_name);
    let before = executable_fingerprint(&installed_executable);

    // `wait_for_executable_unlock` 只表示旧进程交出了文件，不等于安装器会成功
    // 替换它；安装结果一律以安装后的实测为准。
    wait_for_executable_unlock(&invocation.executable, Duration::from_secs(180), log_path)?;
    append_update_log(log_path, "Installed executable lock released");
    let _verified_installer_lock = open_verified_windows_installer(invocation)?;
    append_update_log(log_path, "Installer integrity verified");

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

    let outcome = verify_installed_update(invocation, expected_version.as_deref(), before);
    append_update_log(
        log_path,
        &format!(
            "Post-install verification: {outcome:?} (installDir={})",
            invocation.install_dir.display()
        ),
    );
    match outcome {
        UpdateInstallOutcome::Updated => Ok(UpdateInstallOutcome::Updated),
        UpdateInstallOutcome::Unverified => Ok(UpdateInstallOutcome::Unverified),
        UpdateInstallOutcome::Failed => Err(format!(
            "安装没有生效：{} 未更新到 v{}，请确认安装目录 {} 可写后重试",
            installed_executable.display(),
            expected_version.unwrap_or_default(),
            invocation.install_dir.display()
        )),
    }
}

#[cfg(target_os = "windows")]
fn open_verified_windows_installer(
    invocation: &UpdateHelperInvocation,
) -> Result<std::fs::File, String> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;

    // Keep this handle alive while NSIS runs. Sharing reads lets Windows load
    // the executable while denying writers and path replacement after the hash
    // check has completed.
    const FILE_SHARE_READ: u32 = 0x00000001;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&invocation.installer)
        .map_err(|error| format!("打开更新安装包失败：{error}"))?;
    let actual_size = file
        .metadata()
        .map_err(|error| format!("读取更新安装包信息失败：{error}"))?
        .len();
    if actual_size != invocation.expected_size {
        return Err(format!(
            "安装包大小校验失败：期望 {} 字节，实际 {} 字节",
            invocation.expected_size, actual_size
        ));
    }

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes_read = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取更新安装包失败：{error}"))?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(read as u64);
        if bytes_read > invocation.expected_size {
            return Err("安装包大小超过更新清单声明".to_string());
        }
        hasher.update(&buffer[..read]);
    }
    if bytes_read != invocation.expected_size {
        return Err(format!(
            "安装包大小校验失败：期望 {} 字节，实际 {} 字节",
            invocation.expected_size, bytes_read
        ));
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(&invocation.expected_sha256) {
        return Err("安装包 SHA-256 校验失败".to_string());
    }
    Ok(file)
}

/// 等待旧实例交出 `Codey.exe`。安装器需要一个无人占用的目标文件，这里等的是
/// "可以独占打开"；超时不直接失败，交给安装后的实测结论决定成败，避免把
/// "杀毒软件偶尔占着不放"当成本次更新彻底失败。
#[cfg(target_os = "windows")]
fn wait_for_executable_unlock(
    path: &Path,
    timeout: Duration,
    log_path: &Path,
) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;

    if !path.exists() {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut reported = false;
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
            append_update_log(
                log_path,
                &format!(
                    "Executable still locked after {}s, attempting install anyway: {error}",
                    timeout.as_secs()
                ),
            );
            return Ok(());
        }
        if !reported {
            reported = true;
            append_update_log(log_path, &format!("Waiting for executable lock: {error}"));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// 助手开始真正干活后，旧助手副本已经没有必要保留；顺手清掉本次之前留下的
/// 副本，避免更新缓存里越积越多整份应用。
#[cfg(target_os = "windows")]
fn cleanup_previous_update_helpers(update_dir: &Path) {
    const KEEP_CURRENT: Duration = Duration::from_secs(5 * 60);
    let Ok(entries) = std::fs::read_dir(update_dir) else {
        return;
    };
    let Some(cutoff) = SystemTime::now().checked_sub(KEEP_CURRENT) else {
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
        if !is_helper {
            continue;
        }
        let older_than_current = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < cutoff);
        if older_than_current {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(target_os = "windows")]
fn restart_codey(invocation: &UpdateHelperInvocation, log_path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    crate::windows_session::clear_admin_launch_settings();
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
    // NSIS 替换完文件后，安装器残留的清理进程（或杀毒软件扫描）会短暂持有
    // 新写入的 Codey.exe；此时启动会报 os error 32/33。给每个候选路径一段
    // 有界的重试窗口，等文件锁真正释放。
    const RESTART_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
    let mut failures = Vec::new();
    for target in candidates {
        if !target.is_file() {
            failures.push(format!("{} 不存在", target.display()));
            continue;
        }
        let current_dir = target.parent().unwrap_or_else(|| Path::new("."));
        let deadline = std::time::Instant::now() + RESTART_RETRY_WINDOW;
        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            // 每次重试都写日志会把锁等待刷成上百行重复记录；记首次与之后的每
            // 十次的进度即可。
            if attempt == 1 || attempt.is_multiple_of(10) {
                append_update_log(
                    log_path,
                    &format!("Restarting Codey: {} (attempt {attempt})", target.display()),
                );
            }
            match std::process::Command::new(&target)
                .current_dir(current_dir)
                .creation_flags(
                    codey_runtime_core::windows_create_no_window()
                        | DETACHED_PROCESS
                        | CREATE_NEW_PROCESS_GROUP,
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let retryable = matches!(error.raw_os_error(), Some(32) | Some(33))
                        && std::time::Instant::now() < deadline;
                    if !retryable {
                        failures.push(format!("{}：{error}", target.display()));
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
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
        let expected_sha256 = "a".repeat(64);
        let parsed = parse_update_helper_invocation([
            OsString::from("helper.exe"),
            OsString::from(UPDATE_HELPER_FLAG),
            OsString::from(r"C:\Users\Test User\updates\Codey setup.exe"),
            OsString::from(r"C:\Users\Test User\Programs\Codey\Codey.exe"),
            OsString::from(r"C:\Users\Test User\Programs\Codey"),
            OsString::from("123456"),
            OsString::from(expected_sha256.as_str()),
        ])
        .unwrap()
        .unwrap();

        assert_eq!(
            parsed,
            UpdateHelperInvocation {
                installer: PathBuf::from(r"C:\Users\Test User\updates\Codey setup.exe"),
                executable: PathBuf::from(r"C:\Users\Test User\Programs\Codey\Codey.exe"),
                install_dir: PathBuf::from(r"C:\Users\Test User\Programs\Codey"),
                expected_size: 123456,
                expected_sha256,
                report_path: None,
            }
        );
        assert_eq!(
            nsis_install_directory_argument(&parsed.install_dir),
            OsString::from(r"/D=C:\Users\Test User\Programs\Codey")
        );
    }

    #[test]
    fn update_helper_report_path_matches_the_file_the_main_process_waits_for() {
        let mut update = PathBuf::from("config");
        update.push("updates");
        update.push("v1.2.3");
        update.push("Codey-setup.exe");
        let config = PathBuf::from("config").join("config.json");

        assert_eq!(
            update_helper_report_path(&update),
            Some(update_install_report_path(&config))
        );
    }

    #[test]
    fn update_helper_accepts_an_explicit_report_path() {
        let expected_sha256 = "b".repeat(64);
        let parsed = parse_update_helper_invocation([
            OsString::from("helper.exe"),
            OsString::from(UPDATE_HELPER_FLAG),
            OsString::from(
                r"C:\Users\Test User\config\updates\v1.2.3\Codey-1.2.3-windows-x64-setup.exe",
            ),
            OsString::from(r"C:\Users\Test User\Programs\Codey\Codey.exe"),
            OsString::from(r"C:\Users\Test User\Programs\Codey"),
            OsString::from("123456"),
            OsString::from(expected_sha256.as_str()),
            OsString::from(r"C:\Users\Test User\config\update-install-report.json"),
        ])
        .unwrap()
        .unwrap();

        assert_eq!(
            parsed.report_path,
            Some(PathBuf::from(
                r"C:\Users\Test User\config\update-install-report.json"
            ))
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

    #[test]
    fn update_helper_rejects_invalid_integrity_arguments() {
        let size_error = parse_update_helper_invocation([
            OsString::from("helper.exe"),
            OsString::from(UPDATE_HELPER_FLAG),
            OsString::from("installer.exe"),
            OsString::from("Codey.exe"),
            OsString::from("install-dir"),
            OsString::from("0"),
            OsString::from("not-a-digest"),
        ])
        .unwrap_err();
        assert!(size_error.contains("安装包大小"));

        let digest_error = parse_update_helper_invocation([
            OsString::from("helper.exe"),
            OsString::from(UPDATE_HELPER_FLAG),
            OsString::from("installer.exe"),
            OsString::from("Codey.exe"),
            OsString::from("install-dir"),
            OsString::from("1"),
            OsString::from("not-a-digest"),
        ])
        .unwrap_err();
        assert!(digest_error.contains("SHA-256"));
    }

    #[test]
    fn install_directory_argument_stays_unquoted_for_nsis() {
        let argument =
            nsis_install_directory_argument(Path::new(r"C:\Users\Test User\Programs\Codey"));

        assert_eq!(
            argument,
            OsString::from(r"/D=C:\Users\Test User\Programs\Codey")
        );
        assert!(!argument.to_string_lossy().contains('"'));
    }

    #[test]
    fn installed_version_marker_accepts_plain_and_decorated_contents() {
        assert_eq!(
            parse_installed_version_marker("1.2.3"),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            parse_installed_version_marker("v1.2.3\n"),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            parse_installed_version_marker("Codey 1.2.3 windows-x64\n"),
            Some("1.2.3".to_string())
        );
        assert_eq!(parse_installed_version_marker(""), None);
        assert_eq!(parse_installed_version_marker("Codey"), None);
        assert_eq!(parse_installed_version_marker("1.2"), None);
    }

    #[test]
    fn installer_name_versions_are_parsed_without_arch_false_positives() {
        assert_eq!(
            version_from_installer_name("Codey-1.2.3-windows-x64-setup.exe"),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            version_from_installer_name("Codey setup 1.2.3.exe"),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            version_from_installer_name("Codey-v1.2.3-windows-x64-setup.EXE"),
            Some("1.2.3".to_string())
        );
        assert_eq!(version_from_installer_name("Codey setup.exe"), None);
        assert_eq!(
            version_from_installer_name("Codey-windows-x64-setup.exe"),
            None
        );
    }

    #[test]
    fn install_report_round_trips_through_the_config_directory() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.json");
        let report = UpdateInstallReport {
            version: "1.2.3".to_string(),
            status: "failed".to_string(),
            message: "安装没有生效".to_string(),
            written_at: 42,
        };

        write_update_install_report(&config_path, &report);

        let path = update_install_report_path(&config_path);
        assert_eq!(path.parent(), Some(directory.path()));
        assert!(path.is_file());
        let loaded = read_update_install_report(&config_path).unwrap();
        assert_eq!(loaded.version, "1.2.3");
        assert_eq!(loaded.status, "failed");
        assert_eq!(loaded.message, "安装没有生效");

        clear_update_install_report(&config_path);
        assert!(!path.is_file());
        assert!(read_update_install_report(&config_path).is_none());
    }

    #[test]
    fn install_outcome_requires_a_matching_marker_and_a_replaced_binary() {
        // 版本标记一致且文件确实被换过，才算安装成功。
        assert_eq!(
            decide_install_outcome(Some("1.2.3"), Some("1.2.3"), true, true),
            UpdateInstallOutcome::Updated
        );
        // 目标目录里根本没有可执行文件：安装没落地。
        assert_eq!(
            decide_install_outcome(Some("1.2.3"), Some("1.2.3"), true, false),
            UpdateInstallOutcome::Failed
        );
        // 装到了别处：目标目录还是旧版本标记。
        assert_eq!(
            decide_install_outcome(Some("1.2.2"), Some("1.2.3"), true, true),
            UpdateInstallOutcome::Failed
        );
    }

    #[test]
    fn install_outcome_without_a_marker_never_claims_certainty() {
        // 旧版本目录没有版本标记，只换过文件时按成功处理。
        assert_eq!(
            decide_install_outcome(None, Some("1.2.3"), true, true),
            UpdateInstallOutcome::Updated
        );
        // 没有任何证据时不谎报成功，也不硬判失败。
        assert_eq!(
            decide_install_outcome(None, Some("1.2.3"), false, true),
            UpdateInstallOutcome::Unverified
        );
        // 标记一致但目标文件既没换过、当前也不在：无法证明装成功。
        assert_eq!(
            decide_install_outcome(Some("1.2.3"), Some("1.2.3"), false, false),
            UpdateInstallOutcome::Unverified
        );
        // 文件名解析不出期望版本时退化为指纹判断。
        assert_eq!(
            decide_install_outcome(Some("1.2.3"), None, true, true),
            UpdateInstallOutcome::Updated
        );
        assert_eq!(
            decide_install_outcome(Some("1.2.3"), None, false, true),
            UpdateInstallOutcome::Unverified
        );
    }

    #[test]
    fn stale_install_reports_are_recognised() {
        let fresh = UpdateInstallReport {
            version: "1.2.3".to_string(),
            status: "failed".to_string(),
            message: String::new(),
            written_at: 1_000,
        };
        let now = 1_000 + UPDATE_INSTALL_REPORT_STALE_AFTER.as_secs();

        assert!(!fresh.is_stale(now));
        assert!(fresh.is_stale(now + 1));
    }
}
