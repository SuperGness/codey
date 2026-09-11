use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
#[cfg(windows)]
use tokio::process::Command;

#[cfg(windows)]
use super::{SpawnedCodex, build_codex_command, reap_child_after_cleanup};
#[cfg(windows)]
use crate::error_log;

#[cfg(windows)]
const WINDOWS_CODEX_STOP_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(windows)]
const WINDOWS_STARTUP_PATCH_FAILURE_STOP_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(windows)]
pub(super) struct WindowsStartupProcess(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl WindowsStartupProcess {
    fn package_full_name(&self) -> Result<String> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::Packaging::Appx::{
            GetPackageFullName, PACKAGE_FULL_NAME_MAX_LENGTH,
        };
        use windows::core::PWSTR;

        let mut name = vec![0u16; PACKAGE_FULL_NAME_MAX_LENGTH as usize + 1];
        let mut length = name.len() as u32;
        unsafe {
            GetPackageFullName(
                HANDLE(self.0.as_raw_handle()),
                &mut length,
                PWSTR(name.as_mut_ptr()),
            )
        }
        .ok()
        .context("读取实际启动的 Windows Store Codex 包标识失败")?;
        String::from_utf16(&name[..length as usize - 1]).map_err(Into::into)
    }

    pub(super) fn open(process_id: u32) -> Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };

        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                process_id,
            )
        }
        .context("打开 Windows Codex 启动进程句柄失败")?;
        // Keep the process object alive through startup so PID reuse cannot
        // change the process being observed and its exit code remains readable.
        Ok(Self(unsafe {
            std::os::windows::io::OwnedHandle::from_raw_handle(handle.0)
        }))
    }

    pub(super) fn exit_code(&self) -> Result<Option<u32>> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

        let handle = HANDLE(self.0.as_raw_handle());
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                unsafe { GetExitCodeProcess(handle, &mut code) }
                    .context("读取 Windows Codex 退出码失败")?;
                Ok(Some(code))
            }
            _ => Err(windows::core::Error::from_win32())
                .context("检测 Windows Codex 启动进程状态失败"),
        }
    }
}

#[cfg(windows)]
pub(super) fn windows_startup_process_details(
    app_dir: &Path,
    process_id: Option<u32>,
) -> serde_json::Value {
    match codey_runtime_core::windows_enumerate_processes() {
        Ok(processes) => serde_json::Value::Array(
            processes
                .iter()
                .filter(|process| {
                    Some(process.process_id) == process_id
                        || process
                            .executable_path
                            .as_deref()
                            .is_some_and(|path| windows_path_is_within(path, app_dir))
                })
                .map(|process| {
                    serde_json::json!({
                        "processId": process.process_id,
                        "parentProcessId": process.parent_process_id,
                        "executableName": process.exe_file,
                        "executablePath": process.executable_path,
                        "creationTime": process.creation_time,
                    })
                })
                .collect(),
        ),
        Err(error) => serde_json::json!({ "queryError": format!("{error:#}") }),
    }
}

#[cfg(any(windows, test))]
fn windows_package_full_name(app_dir: &Path) -> Option<String> {
    codey_runtime_core::app_paths::packaged_app_user_model_id(app_dir)?;
    let path = app_dir.to_string_lossy().replace('\\', "/");
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let mut package_name = parts.next_back()?;
    if package_name.eq_ignore_ascii_case("app") {
        package_name = parts.next_back()?;
    }
    Some(package_name.to_string())
}

#[cfg(any(windows, test))]
#[derive(Debug)]
pub(super) struct WindowsPackageChanged;

#[cfg(any(windows, test))]
impl std::fmt::Display for WindowsPackageChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Windows Store Codex 包已更新，需要重新准备启动环境")
    }
}

#[cfg(any(windows, test))]
impl std::error::Error for WindowsPackageChanged {}

#[cfg(windows)]
fn registered_windows_packages(package_full_name: &str) -> Result<Vec<String>> {
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Storage::Packaging::Appx::{
        FindPackagesByPackageFamily, PACKAGE_FILTER_HEAD,
    };
    use windows::core::{PCWSTR, PWSTR};

    let app_id =
        codey_runtime_core::app_paths::packaged_app_user_model_id(Path::new(package_full_name))
            .context("无法识别 Windows Store Codex 包标识")?;
    let (family, _) = app_id
        .split_once('!')
        .context("Windows Store Codex 包标识无效")?;
    let family = family.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut count = 0;
    let mut length = 0;
    let status = unsafe {
        FindPackagesByPackageFamily(
            PCWSTR(family.as_ptr()),
            PACKAGE_FILTER_HEAD,
            &mut count,
            None,
            &mut length,
            PWSTR::null(),
            None,
        )
    };
    if status != ERROR_INSUFFICIENT_BUFFER {
        status.ok().context("查询当前用户的 Codex 注册包失败")?;
        return Ok(Vec::new());
    }
    let mut names = vec![PWSTR::null(); count as usize];
    let mut buffer = vec![0u16; length as usize];
    unsafe {
        FindPackagesByPackageFamily(
            PCWSTR(family.as_ptr()),
            PACKAGE_FILTER_HEAD,
            &mut count,
            Some(names.as_mut_ptr()),
            &mut length,
            PWSTR(buffer.as_mut_ptr()),
            None,
        )
    }
    .ok()
    .context("读取当前用户的 Codex 注册包失败")?;
    names[..count as usize]
        .iter()
        .map(|name| unsafe { name.to_string() }.map_err(Into::into))
        .collect()
}

#[cfg(windows)]
pub(super) fn refresh_windows_packaged_app_dir(app_dir: &Path) -> Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Storage::Packaging::Appx::GetPackagePathByFullName;
    use windows::core::{PCWSTR, PWSTR};

    let Some(previous) = windows_package_full_name(app_dir) else {
        return Ok(app_dir.to_path_buf());
    };
    let packages = registered_windows_packages(&previous)?;
    anyhow::ensure!(
        packages.len() == 1,
        "无法唯一确定当前用户的 Windows Store Codex 注册包"
    );
    let current = &packages[0];
    let name = current.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut length = 0;
    let status =
        unsafe { GetPackagePathByFullName(PCWSTR(name.as_ptr()), &mut length, PWSTR::null()) };
    if status != ERROR_INSUFFICIENT_BUFFER {
        status
            .ok()
            .context("查询 Windows Store Codex 安装路径失败")?;
    }
    anyhow::ensure!(length > 1, "Windows Store Codex 安装路径为空");
    let mut buffer = vec![0u16; length as usize];
    unsafe {
        GetPackagePathByFullName(
            PCWSTR(name.as_ptr()),
            &mut length,
            PWSTR(buffer.as_mut_ptr()),
        )
    }
    .ok()
    .context("读取 Windows Store Codex 安装路径失败")?;
    let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length as usize - 1],
    ));
    let path = codey_runtime_core::app_paths::normalize_codex_app_path(&path)
        .context("当前 Windows Store Codex 包中未找到启动程序")?;
    if previous != *current {
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.windows_package_refreshed",
            serde_json::json!({ "previousPackage": previous, "package": current }),
        );
    }
    Ok(path)
}

#[cfg(any(windows, test))]
fn windows_package_was_replaced(previous: &str, registered: &[String]) -> bool {
    let family = codey_runtime_core::app_paths::packaged_app_user_model_id(Path::new(previous));
    family.is_some()
        && !registered.is_empty()
        && registered.iter().all(|current| {
            current != previous
                && codey_runtime_core::app_paths::packaged_app_user_model_id(Path::new(current))
                    == family
        })
}

#[cfg(any(windows, test))]
fn windows_environment_block(environment: &[(String, String)]) -> Result<Vec<u16>> {
    let mut entries = environment
        .iter()
        .map(|(name, value)| {
            anyhow::ensure!(
                !name.is_empty() && !name.contains(['=', '\0']) && !value.contains('\0'),
                "Windows Codex 兼容环境包含无效字符"
            );
            Ok(format!("{name}={value}"))
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.to_ascii_uppercase());
    let mut block = Vec::new();
    for entry in entries {
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(windows)]
pub(super) struct WindowsPackageDebugSession {
    package_full_name: Option<String>,
}

#[cfg(windows)]
impl WindowsPackageDebugSession {
    fn start(app_dir: &Path, environment: &[(String, String)]) -> Result<Self> {
        let package_full_name =
            windows_package_full_name(app_dir).context("无法识别 Windows Store Codex 包全名")?;
        enable_windows_packaged_environment(&package_full_name, environment)?;
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.windows_package_environment_enabled",
            serde_json::json!({ "package": package_full_name }),
        );
        Ok(Self {
            package_full_name: Some(package_full_name),
        })
    }

    pub(super) fn finish(mut self) -> Result<()> {
        let package_full_name = self
            .package_full_name
            .as_deref()
            .expect("active package debug session should have a package name");
        disable_windows_packaged_environment(package_full_name)?;
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.windows_package_environment_cleared",
            serde_json::json!({ "package": package_full_name }),
        );
        self.package_full_name.take();
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsPackageDebugSession {
    fn drop(&mut self) {
        if let Some(package_full_name) = self.package_full_name.take() {
            let _ = disable_windows_packaged_environment(&package_full_name);
        }
    }
}

#[cfg(windows)]
fn with_windows_package_debug_settings<T>(
    operation: impl FnOnce(
        &windows::Win32::UI::Shell::IPackageDebugSettings,
    ) -> windows::core::Result<T>,
) -> Result<T> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::UI::Shell::{IPackageDebugSettings, PackageDebugSettings};

    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let should_uninitialize = initialized.is_ok();
        initialized.ok().or_else(|error| {
            const RPC_E_CHANGED_MODE: i32 = -2147417850;
            if error.code().0 == RPC_E_CHANGED_MODE {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        let result = (|| {
            let settings: IPackageDebugSettings =
                CoCreateInstance(&PackageDebugSettings, None, CLSCTX_INPROC_SERVER)?;
            operation(&settings)
        })();
        if should_uninitialize {
            CoUninitialize();
        }
        result.map_err(Into::into)
    }
}

#[cfg(windows)]
fn enable_windows_packaged_environment(
    package_full_name: &str,
    environment: &[(String, String)],
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;

    let package_full_name = package_full_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let executable = std::env::current_exe().context("定位 Codey 包启动恢复助手失败")?;
    let mut debugger_command = vec![u16::from(b'"')];
    debugger_command.extend(executable.as_os_str().encode_wide());
    debugger_command.extend(
        format!(
            "\" {}",
            crate::codex_startup_patch::WINDOWS_PACKAGE_RESUME_ARGUMENT
        )
        .encode_utf16(),
    );
    debugger_command.push(0);
    let environment = windows_environment_block(environment)?;

    with_windows_package_debug_settings(|settings| unsafe {
        let package = PCWSTR(package_full_name.as_ptr());
        settings.DisableDebugging(package)?;
        settings.EnableDebugging(
            package,
            PCWSTR(debugger_command.as_ptr()),
            PCWSTR(environment.as_ptr()),
        )
    })
    .context("为 Windows Store Codex 安装一次性 CLI 兼容环境失败")
}

#[cfg(windows)]
fn disable_windows_packaged_environment(package_full_name: &str) -> Result<()> {
    use windows::core::PCWSTR;

    let name = package_full_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = with_windows_package_debug_settings(|settings| unsafe {
        settings.DisableDebugging(PCWSTR(name.as_ptr()))
    });
    if result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<windows::core::Error>())
        .is_some_and(|error| {
            error.code() == windows::Win32::Foundation::ERROR_NOT_FOUND.to_hresult()
        })
    {
        // An update can unregister the old package between EnableDebugging and cleanup.
        // Query failures and an unchanged registration must remain fatal.
        let registered = registered_windows_packages(package_full_name)?;
        if windows_package_was_replaced(package_full_name, &registered) {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.windows_package_cleanup_after_update",
                serde_json::json!({ "previousPackage": package_full_name, "registeredPackages": registered }),
            );
            return Ok(());
        }
    }
    result.context("清理 Windows Store Codex 一次性 CLI 兼容环境失败")
}

#[cfg(any(windows, test))]
pub(super) fn normalized_windows_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_ascii_lowercase()
}

#[cfg(windows)]
pub(super) async fn spawn_windows_codex(
    app_dir: &std::path::Path,
    debug_port: u16,
    extra_args: &[String],
    environment: &[(String, String)],
    require_wrapper_environment: bool,
) -> Result<(SpawnedCodex, Option<WindowsPackageDebugSession>, bool)> {
    anyhow::ensure!(
        !require_wrapper_environment || !environment.is_empty(),
        "Codex CLI 兼容入口缺少运行环境，已停止启动"
    );
    if let Some(activation) =
        codey_runtime_core::launcher::build_packaged_activation(app_dir, debug_port, extra_args)
        && let codey_runtime_core::launcher::CodexLaunch::PackagedActivation {
            app_user_model_id,
            arguments,
            ..
        } = activation
    {
        let package_debug_session = if environment.is_empty() {
            None
        } else {
            match WindowsPackageDebugSession::start(app_dir, environment) {
                Ok(session) => Some(session),
                Err(error) => {
                    let package_name = windows_package_full_name(app_dir)
                        .context("无法识别待清理的 Windows Store Codex 包全名")?;
                    if let Err(cleanup) = disable_windows_packaged_environment(&package_name) {
                        return Err(startup_activation_error_after_cleanup(
                            error,
                            Ok(()),
                            Err(cleanup),
                        ));
                    }
                    if windows_package_was_replaced(
                        &package_name,
                        &registered_windows_packages(&package_name)?,
                    ) {
                        return Err(WindowsPackageChanged.into());
                    }
                    if require_wrapper_environment {
                        return Err(error)
                            .context("Codex CLI 兼容入口无法应用运行环境，已停止启动");
                    }
                    error_log::record_failure(
                        "compatibility_fallback",
                        "enable_windows_packaged_cli_environment",
                        format!("{error:#}"),
                        serde_json::json!({ "appPath": app_dir }),
                    );
                    None
                }
            }
        };
        let environment_applied = package_debug_session.is_some();
        let activation_result = async {
            let existing_process_ids = codey_runtime_core::windows_enumerate_processes()
                .context("检测 Windows Store 激活前的已有进程失败")?
                .into_iter()
                .map(|process| process.process_id)
                .collect::<HashSet<_>>();
            let mut process_id =
                codey_runtime_core::launcher::activate_packaged_app(&app_user_model_id, &arguments)
                    .await?;
            if activation_reused_existing_process(&existing_process_ids, process_id) {
                // ActivateApplication can return an existing single instance.
                // Its original command line cannot carry this launch's ports.
                terminate_windows_codex_processes(app_dir, Some(process_id))
                    .await
                    .context("停止被 Windows Store 激活复用的旧 Codex 实例失败")?;
                let retry_existing_process_ids = codey_runtime_core::windows_enumerate_processes()
                    .context("检测 Windows Store 重新激活前的已有进程失败")?
                    .into_iter()
                    .map(|process| process.process_id)
                    .collect::<HashSet<_>>();
                process_id = codey_runtime_core::launcher::activate_packaged_app(
                    &app_user_model_id, &arguments,
                ).await.context("重新激活 Windows Store Codex 失败")?;
                if activation_reused_existing_process(&retry_existing_process_ids, process_id) {
                    anyhow::bail!(
                        "Windows Store Codex 再次复用了已有进程 {process_id}，本次 CDP 启动参数未能可靠生效"
                    );
                }
            }
            Ok::<_, anyhow::Error>(process_id)
        }
        .await;
        let process_id = match activation_result {
            Ok(process_id) => process_id,
            Err(error) => {
                // Activation may start a process even when it returns an error.
                // Cleanup must be confirmed before the outer loop can retry.
                let stopped = terminate_windows_codex_processes_with_timeout(
                    app_dir,
                    None,
                    WINDOWS_STARTUP_PATCH_FAILURE_STOP_TIMEOUT,
                )
                .await;
                let cleared = package_debug_session
                    .map(WindowsPackageDebugSession::finish)
                    .transpose();
                return Err(startup_activation_error_after_cleanup(
                    error,
                    stopped,
                    cleared.map(|_| ()),
                ));
            }
        };
        let startup_process = match WindowsStartupProcess::open(process_id) {
            Ok(process) => Some(process),
            Err(error) => {
                let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                    "launcher.process_probe_failed",
                    serde_json::json!({ "processId": process_id, "detail": format!("{error:#}") }),
                );
                None
            }
        };
        let package_check = startup_process.as_ref()
            .context("无法确认实际启动的 Windows Store Codex 包")
            .and_then(WindowsStartupProcess::package_full_name)
            .and_then(|actual| {
                if windows_package_full_name(app_dir).as_deref() == Some(actual.as_str()) {
                    Ok(())
                } else {
                    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                        "launcher.windows_package_changed_during_activation",
                        serde_json::json!({ "expectedPackage": windows_package_full_name(app_dir), "actualPackage": actual, "processId": process_id }),
                    );
                    Err(WindowsPackageChanged.into())
                }
            });
        if let Err(error) = package_check {
            let stopped = terminate_windows_codex_processes_with_timeout(
                app_dir,
                Some(process_id),
                WINDOWS_STARTUP_PATCH_FAILURE_STOP_TIMEOUT,
            )
            .await;
            let cleared = package_debug_session
                .map(WindowsPackageDebugSession::finish)
                .transpose();
            return Err(startup_activation_error_after_cleanup(
                error,
                stopped,
                cleared.map(|_| ()),
            ));
        }
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.windows_package_activated",
            serde_json::json!({
                "processId": process_id,
                "wrapperEnvironmentApplied": environment_applied,
                "processHandleCaptured": startup_process.is_some(),
                "processes": windows_startup_process_details(app_dir, Some(process_id)),
            }),
        );
        return Ok((
            SpawnedCodex {
                child: None,
                process_id: Some(process_id),
                startup_process,
                performance_status: String::new(),
                performance_detail: String::new(),
                startup_injection_mode: String::new(),
            },
            package_debug_session,
            environment_applied,
        ));
    }

    let command = build_codex_command(app_dir, debug_port, extra_args);
    let executable = command
        .first()
        .ok_or_else(|| anyhow::anyhow!("Codex 启动命令为空"))?;
    let mut child_command = Command::new(executable);
    child_command.args(&command[1..]);
    child_command.envs(environment.iter().map(|(name, value)| (name, value)));
    // A stale WSL_DISTRO_NAME inherited by the native Windows app makes
    // current Codex builds synchronously probe wsl.exe during startup.
    child_command.env_remove("WSL_DISTRO_NAME");
    child_command.creation_flags(codey_runtime_core::windows_create_no_window());
    let child = child_command
        .spawn()
        .with_context(|| format!("启动 Codex 失败：{executable}"))?;
    let process_id = child.id();
    Ok((
        SpawnedCodex {
            child: Some(child),
            process_id,
            startup_process: None,
            performance_status: String::new(),
            performance_detail: String::new(),
            startup_injection_mode: String::new(),
        },
        None,
        !environment.is_empty(),
    ))
}

#[cfg(any(windows, test))]
pub(super) fn startup_activation_error_after_cleanup(
    error: anyhow::Error,
    stopped: Result<()>,
    cleared: Result<()>,
) -> anyhow::Error {
    match (stopped, cleared) {
        (Ok(()), Ok(())) => error,
        (stopped, cleared) => anyhow::anyhow!(
            "{error:#}；Windows 启动清理未完成，已停止重试；进程：{}；兼容环境：{}",
            stopped
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "已清理".into()),
            cleared
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "已清理".into()),
        ),
    }
}

#[cfg(any(windows, test))]
pub(super) fn activation_reused_existing_process(
    existing_process_ids: &HashSet<u32>,
    process_id: u32,
) -> bool {
    existing_process_ids.contains(&process_id)
}

#[cfg(any(windows, test))]
pub(super) fn process_creation_identity_matches(
    expected_creation_time: Option<u64>,
    actual_creation_time: Option<u64>,
) -> bool {
    match (expected_creation_time, actual_creation_time) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

#[cfg(windows)]
pub(super) async fn stop_windows_spawned_codex(
    spawned: &mut SpawnedCodex,
    app_dir: &std::path::Path,
) -> Result<()> {
    let process_id = spawned.process_id.take();
    let process_stop = terminate_windows_codex_processes_with_timeout(
        app_dir,
        process_id,
        WINDOWS_STARTUP_PATCH_FAILURE_STOP_TIMEOUT,
    )
    .await;
    if let Some(child) = spawned.child.take() {
        reap_child_after_cleanup(child, "reap_child_after_startup_patch_failure").await;
    }
    if let Err(error) = &process_stop {
        error_log::record_failure(
            "cleanup_failed",
            "cleanup_windows_after_startup_patch_failure",
            format!("{error:#}"),
            serde_json::json!({
                "appPath": app_dir,
                "processId": process_id,
            }),
        );
        eprintln!("Codex 启动失败后的进程清理失败：{error:#}");
    }
    process_stop
}

#[cfg(target_os = "macos")]
pub(super) fn build_fresh_macos_open_command(
    app_dir: &std::path::Path,
    debug_port: u16,
    extra_args: &[String],
) -> Vec<String> {
    let mut command =
        codey_runtime_core::launcher::build_macos_open_command(app_dir, debug_port, extra_args);
    if command.first().map(String::as_str) == Some("open")
        && !command.iter().any(|part| part == "-n" || part == "--new")
    {
        command.insert(1, "-n".to_string());
    }
    command
}

#[cfg(target_os = "macos")]
pub(super) async fn stop_macos_codex(
    inspector_argument: &str,
    app_dir: &std::path::Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
) -> Result<()> {
    terminate_unix_codex_processes(
        app_dir,
        process_id,
        process_group_id,
        Some(inspector_argument),
    )
    .await
    .map(|_| ())
}

#[cfg(unix)]
pub(super) fn owned_unix_codex_process_ids(
    processes: &[crate::process_tree::UnixProcessInfo],
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> HashSet<u32> {
    let current_process_id = std::process::id();
    let roots = processes.iter().filter_map(|process| {
        let matches_root = Some(process.process_id) == process_id
            || Some(process.process_group_id) == process_group_id
            || crate::process_tree::command_uses_path(&process.command, app_dir)
            || launch_marker.is_some_and(|marker| {
                crate::process_tree::command_has_argument(&process.command, marker)
            });
        matches_root.then_some(process.process_id)
    });
    crate::process_tree::process_ids_with_descendants(processes, roots, current_process_id)
}

#[cfg(unix)]
fn owned_unix_process_group(
    processes: &[crate::process_tree::UnixProcessInfo],
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> Option<u32> {
    let process_group_id = process_group_id?;
    processes
        .iter()
        .any(|process| {
            process.process_group_id == process_group_id
                && (Some(process.process_id) == process_id
                    || crate::process_tree::command_uses_path(&process.command, app_dir)
                    || launch_marker.is_some_and(|marker| {
                        crate::process_tree::command_has_argument(&process.command, marker)
                    }))
        })
        .then_some(process_group_id)
}

#[cfg(unix)]
pub(super) async fn terminate_unix_codex_processes(
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> Result<usize> {
    let mut known_processes = HashMap::new();
    let mut processes = crate::process_tree::unix_process_snapshot().await?;
    let initially_owned = owned_unix_codex_process_ids(
        &processes,
        app_dir,
        process_id,
        process_group_id,
        launch_marker,
    );
    known_processes.extend(crate::process_tree::identities_for_process_ids(
        &processes,
        &initially_owned,
    ));

    let owned_process_group = owned_unix_process_group(
        &processes,
        app_dir,
        process_id,
        process_group_id,
        launch_marker,
    );
    crate::process_tree::signal_process_group(owned_process_group, libc::SIGTERM)?;
    crate::process_tree::signal_processes(
        &known_processes.keys().copied().collect(),
        libc::SIGTERM,
    )?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let poll_delays = [
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_millis(350),
        Duration::from_millis(550),
        Duration::from_millis(800),
    ];
    let mut poll_index = 0usize;
    let remaining = loop {
        let currently_owned = owned_unix_codex_process_ids(
            &processes,
            app_dir,
            process_id,
            process_group_id,
            launch_marker,
        );
        let newly_discovered = currently_owned
            .into_iter()
            .filter(|process_id| !known_processes.contains_key(process_id))
            .collect::<HashSet<_>>();
        if !newly_discovered.is_empty() {
            crate::process_tree::signal_processes(&newly_discovered, libc::SIGTERM)?;
            known_processes.extend(crate::process_tree::identities_for_process_ids(
                &processes,
                &newly_discovered,
            ));
        }
        let remaining = crate::process_tree::matching_process_ids(&processes, &known_processes);
        if remaining.is_empty() || tokio::time::Instant::now() >= deadline {
            break remaining;
        }
        let remaining_time = deadline.saturating_duration_since(tokio::time::Instant::now());
        let delay = poll_delays
            .get(poll_index)
            .copied()
            .unwrap_or(Duration::from_millis(800))
            .min(remaining_time);
        poll_index = poll_index.saturating_add(1);
        tokio::time::sleep(delay).await;
        processes = crate::process_tree::unix_process_snapshot().await?;
    };

    if !remaining.is_empty() {
        let owned_process_group = process_group_id.filter(|process_group_id| {
            processes.iter().any(|process| {
                process.process_group_id == *process_group_id
                    && remaining.contains(&process.process_id)
            })
        });
        crate::process_tree::signal_process_group(owned_process_group, libc::SIGKILL)?;
        crate::process_tree::signal_processes(&remaining, libc::SIGKILL)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let final_snapshot = crate::process_tree::unix_process_snapshot().await?;
        let live_process_ids =
            crate::process_tree::matching_process_ids(&final_snapshot, &known_processes);
        let stubborn_processes = remaining
            .intersection(&live_process_ids)
            .copied()
            .collect::<Vec<_>>();
        if !stubborn_processes.is_empty() {
            anyhow::bail!("强制停止 Codex 进程超时：{stubborn_processes:?}");
        }
    }
    Ok(known_processes.len())
}

#[cfg(target_os = "macos")]
pub(super) fn macos_main_executable_is_running(
    processes: &[crate::process_tree::UnixProcessInfo],
    executable: &std::path::Path,
) -> bool {
    processes
        .iter()
        .any(|process| crate::process_tree::command_uses_path(&process.command, executable))
}

#[cfg(target_os = "macos")]
pub(super) async fn macos_codex_is_running(app_dir: &std::path::Path) -> Result<bool> {
    // 启动前只检查 App 的主可执行文件，忽略 app-server 和 Chromium helper。
    let executable = codey_runtime_core::app_paths::build_codex_executable(app_dir);
    let processes = crate::process_tree::unix_process_snapshot().await?;
    Ok(macos_main_executable_is_running(&processes, &executable))
}

#[cfg(any(windows, test))]
fn windows_path_is_within(path: &Path, directory: &Path) -> bool {
    let path = normalized_windows_path(path);
    let directory = normalized_windows_path(directory);
    path == directory
        || path
            .strip_prefix(&directory)
            .is_some_and(|rest| rest.starts_with('\\'))
}

#[cfg(any(windows, test))]
pub(super) fn windows_owned_process_ids_from_snapshot<'a>(
    app_dir: &Path,
    process_id: Option<u32>,
    processes: impl IntoIterator<Item = (u32, u32, Option<&'a Path>)>,
) -> HashSet<u32> {
    let processes = processes.into_iter().collect::<Vec<_>>();
    let mut process_ids = processes
        .iter()
        .filter(|(candidate_process_id, _, executable_path)| {
            Some(*candidate_process_id) == process_id
                || executable_path.is_some_and(|path| windows_path_is_within(path, app_dir))
        })
        .map(|(candidate_process_id, _, _)| *candidate_process_id)
        .collect::<HashSet<_>>();
    windows_extend_tracked_descendants_from_snapshot(&mut process_ids, processes);
    process_ids
}

#[cfg(any(windows, test))]
pub(super) fn windows_extend_tracked_descendants_from_snapshot<'a>(
    process_ids: &mut HashSet<u32>,
    processes: impl IntoIterator<Item = (u32, u32, Option<&'a Path>)>,
) {
    let processes = processes.into_iter().collect::<Vec<_>>();
    loop {
        let previous_len = process_ids.len();
        for (candidate_process_id, parent_process_id, _) in &processes {
            if process_ids.contains(parent_process_id) {
                process_ids.insert(*candidate_process_id);
            }
        }
        if process_ids.len() == previous_len {
            break;
        }
    }
}

#[cfg(windows)]
pub(super) async fn terminate_windows_codex_processes(
    app_dir: &Path,
    process_id: Option<u32>,
) -> Result<()> {
    terminate_windows_codex_processes_with_timeout(app_dir, process_id, WINDOWS_CODEX_STOP_TIMEOUT)
        .await
}

#[cfg(windows)]
async fn terminate_windows_codex_processes_with_timeout(
    app_dir: &Path,
    process_id: Option<u32>,
    stop_timeout: Duration,
) -> Result<()> {
    terminate_windows_codex_processes_with_snapshot(
        app_dir,
        process_id,
        stop_timeout,
        codey_runtime_core::windows_enumerate_processes,
    )
    .await
}

#[cfg(windows)]
async fn terminate_windows_codex_processes_with_snapshot(
    app_dir: &Path,
    process_id: Option<u32>,
    stop_timeout: Duration,
    mut snapshot: impl FnMut() -> Result<Vec<codey_runtime_core::WindowsProcessInfo>>,
) -> Result<()> {
    let processes = snapshot().context("检测待停止的 Windows Codex 进程失败")?;
    let mut process_ids = windows_owned_process_ids_from_snapshot(
        app_dir,
        process_id,
        processes.iter().map(|process| {
            (
                process.process_id,
                process.parent_process_id,
                process.executable_path.as_deref(),
            )
        }),
    );
    process_ids.remove(&std::process::id());
    let mut ordered_process_ids = process_ids.iter().copied().collect::<Vec<_>>();
    ordered_process_ids.sort_by_key(|candidate| {
        let mut depth = 0_usize;
        let mut current = *candidate;
        while let Some(parent_process_id) = processes
            .iter()
            .find(|process| process.process_id == current)
            .map(|process| process.parent_process_id)
            .filter(|parent_process_id| process_ids.contains(parent_process_id))
        {
            depth = depth.saturating_add(1);
            current = parent_process_id;
        }
        std::cmp::Reverse(depth)
    });
    let mut expected_creation_times = process_ids
        .iter()
        .filter_map(|process_id| {
            processes
                .iter()
                .find(|process| process.process_id == *process_id)
                .map(|process| (*process_id, process.creation_time))
        })
        .collect::<HashMap<_, _>>();
    for process_id in ordered_process_ids {
        let _terminated_natively = processes
            .iter()
            .find(|process| process.process_id == process_id)
            .is_some_and(
                |process| match (&process.executable_path, process.creation_time) {
                    (Some(path), Some(creation_time)) => {
                        codey_runtime_core::windows_terminate_process_if_matches(
                            process.process_id,
                            path,
                            creation_time,
                        )
                    }
                    (_, Some(creation_time)) => {
                        codey_runtime_core::windows_terminate_process_if_creation_matches(
                            process.process_id,
                            creation_time,
                        )
                    }
                    _ => false,
                },
            );
    }
    let deadline = tokio::time::Instant::now() + stop_timeout;
    loop {
        let current_processes = snapshot().context("确认 Windows Codex 进程清理结果失败")?;
        let previous_process_ids = process_ids.clone();
        windows_extend_tracked_descendants_from_snapshot(
            &mut process_ids,
            current_processes.iter().map(|process| {
                (
                    process.process_id,
                    process.parent_process_id,
                    process.executable_path.as_deref(),
                )
            }),
        );
        for discovered_process_id in process_ids.difference(&previous_process_ids) {
            let Some(process) = current_processes
                .iter()
                .find(|process| process.process_id == *discovered_process_id)
            else {
                continue;
            };
            expected_creation_times.insert(*discovered_process_id, process.creation_time);
            match (&process.executable_path, process.creation_time) {
                (Some(path), Some(creation_time)) => {
                    let _terminated_natively =
                        codey_runtime_core::windows_terminate_process_if_matches(
                            process.process_id,
                            path,
                            creation_time,
                        );
                }
                (_, Some(creation_time)) => {
                    let _terminated_natively =
                        codey_runtime_core::windows_terminate_process_if_creation_matches(
                            process.process_id,
                            creation_time,
                        );
                }
                _ => {}
            }
        }
        let current = current_processes
            .iter()
            .filter(|process| process_ids.contains(&process.process_id))
            .map(|process| {
                (
                    process.process_id,
                    process.exe_file.clone(),
                    process.creation_time,
                )
            })
            .collect::<Vec<_>>();
        let remaining = windows_stop_survivors(&expected_creation_times, &current, &process_ids);
        if remaining.is_empty() {
            return Ok(());
        }
        if tokio::time::Instant::now() < deadline {
            // Windows exits lag behind TerminateProcess (pending I/O,
            // throttled children, antivirus scans) and a first attempt can
            // race with process teardown. Re-issue creation-identity-checked
            // terminations while waiting instead of giving up on survivors.
            for (process_id, _, expected_creation_time) in &remaining {
                if let Some(expected_creation_time) = expected_creation_time {
                    let _terminated_again =
                        codey_runtime_core::windows_terminate_process_if_creation_matches(
                            *process_id,
                            *expected_creation_time,
                        );
                }
            }
        } else {
            anyhow::bail!(
                "无法安全停止 Windows Codex 进程：{}",
                windows_stop_failure_summary(&remaining),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(any(windows, test))]
pub(super) fn windows_stop_survivors(
    expected_creation_times: &HashMap<u32, Option<u64>>,
    current: &[(u32, String, Option<u64>)],
    targets: &HashSet<u32>,
) -> Vec<(u32, String, Option<u64>)> {
    current
        .iter()
        .filter(|(process_id, _, creation_time)| {
            targets.contains(process_id)
                && expected_creation_times
                    .get(process_id)
                    .is_some_and(|expected| {
                        process_creation_identity_matches(*expected, *creation_time)
                    })
        })
        .map(|(process_id, exe_file, _)| {
            // Retry termination must retain the identity captured before the
            // first attempt. A timestamp learned only after that attempt could
            // belong to a different process that has reused the target pid.
            let expected_creation_time = expected_creation_times.get(process_id).copied().flatten();
            (*process_id, exe_file.clone(), expected_creation_time)
        })
        .collect()
}

#[cfg(any(windows, test))]
pub(super) fn windows_stop_failure_summary(remaining: &[(u32, String, Option<u64>)]) -> String {
    const MAX_LISTED: usize = 5;
    let listed = remaining
        .iter()
        .take(MAX_LISTED)
        .map(|(process_id, exe_file, _)| format!("{exe_file}({process_id})"))
        .collect::<Vec<_>>()
        .join("、");
    if remaining.len() > MAX_LISTED {
        format!(
            "{} 个进程仍在运行：{listed} 等共 {} 个",
            remaining.len(),
            remaining.len()
        )
    } else {
        format!("{} 个进程仍在运行：{listed}", remaining.len())
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn package_cleanup_requires_confirmed_replacement_in_the_same_family() {
        let old = "OpenAI.Codex_26.901.6511.0_x64__2p2nqsd0c76g0";
        let new = "OpenAI.Codex_26.903.8094.0_x64__2p2nqsd0c76g0";
        assert!(windows_package_was_replaced(old, &[new.into()]));
        assert!(!windows_package_was_replaced(
            old,
            &[old.into(), new.into()]
        ));
        assert!(!windows_package_was_replaced(old, &[]));
        assert!(!windows_package_was_replaced(
            old,
            &[new.replace("Codex_", "CodexBeta_")]
        ));
        assert!(!windows_package_was_replaced(
            old,
            &[new.replace("2p2nqsd0c76g0", "otherpublisher")]
        ));
        assert!(!windows_package_was_replaced(old, &["invalid".into()]));
    }

    #[cfg(windows)]
    #[test]
    fn store_process_handle_retains_exit_code_after_child_is_reaped() {
        use std::io::Write;
        for code in [37, 259] {
            let mut child = std::process::Command::new("cmd")
                .args(["/d", "/c", &format!("set /p value= & exit /b {code}")])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let process = WindowsStartupProcess::open(child.id()).unwrap();
            assert_eq!(process.exit_code().unwrap(), None);
            child.stdin.take().unwrap().write_all(b"done\r\n").unwrap();
            child.wait().unwrap();
            drop(child);
            // 259 is STILL_ACTIVE only for a running process; it is also a valid exit code.
            assert_eq!(process.exit_code().unwrap(), Some(code));
        }
    }

    // 【自动化测试】Windows 清理 - 初始或确认快照失败不得报告清理成功
    #[cfg(windows)]
    #[tokio::test]
    async fn cleanup_snapshot_failure_is_not_success() {
        for fail_on in [1, 2] {
            let mut calls = 0;
            let result = terminate_windows_codex_processes_with_snapshot(
                Path::new(r"C:\CodeyTestMissingApp"),
                None,
                Duration::ZERO,
                || {
                    calls += 1;
                    if calls == fail_on {
                        anyhow::bail!("snapshot unavailable")
                    }
                    Ok(Vec::new())
                },
            )
            .await;
            assert!(format!("{:#}", result.unwrap_err()).contains("snapshot unavailable"));
            assert_eq!(calls, fail_on);
        }
    }

    #[test]
    fn windows_packaged_cli_environment_is_valid_and_scoped_to_the_codex_package() {
        let app_dir = Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.901.20858.0_x64__2p2nqsd0c76g0\app",
        );
        assert_eq!(
            windows_package_full_name(app_dir).as_deref(),
            Some("OpenAI.Codex_26.901.20858.0_x64__2p2nqsd0c76g0")
        );

        let block = windows_environment_block(&[
            ("B".to_string(), "two".to_string()),
            ("A".to_string(), "1".to_string()),
        ])
        .unwrap();
        assert_eq!(block, "A=1\0B=two\0\0".encode_utf16().collect::<Vec<_>>());
    }
}
