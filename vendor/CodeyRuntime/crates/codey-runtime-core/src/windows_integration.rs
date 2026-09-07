#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::iter::once;
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::OnceLock;

#[cfg(windows)]
use windows::Win32::Foundation::{
    BOOL, CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, FILETIME, HANDLE, HWND,
    LPARAM, MAX_PATH, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
#[cfg(windows)]
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
#[cfg(windows)]
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
#[cfg(windows)]
use windows::Win32::UI::Shell::{ExtractIconExW, ShellExecuteW};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWMINNOACTIVE;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongPtrW, GetWindowTextLengthW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOW, SetForegroundWindow,
    ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    HICON, ICON_BIG, ICON_SMALL, SendMessageW, WM_SETICON,
};
#[cfg(windows)]
use windows::core::{PCWSTR, PROPVARIANT, PWSTR};

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsProcessInfo {
    pub process_id: u32,
    pub parent_process_id: u32,
    pub exe_file: String,
    pub executable_path: Option<PathBuf>,
    pub creation_time: Option<u64>,
}

#[cfg(windows)]
pub fn open_url(url: &str) -> anyhow::Result<()> {
    let operation = wide_null("open");
    let file = wide_null(url);
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWMINNOACTIVE,
        )
    };
    let code = result.0 as isize;
    if code <= 32 {
        anyhow::bail!("ShellExecuteW returned {code}");
    }
    Ok(())
}

#[cfg(windows)]
pub fn enumerate_processes() -> anyhow::Result<Vec<WindowsProcessInfo>> {
    retry_process_snapshot(enumerate_processes_once)
}

#[cfg(any(windows, test))]
fn retry_process_snapshot<T>(mut snapshot: impl FnMut() -> anyhow::Result<T>) -> anyhow::Result<T> {
    // Discard incomplete snapshots and retry transient Windows query failures.
    for attempt in 0..3 {
        match snapshot() {
            Ok(processes) => return Ok(processes),
            Err(error) if attempt == 2 => return Err(error),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    unreachable!()
}

#[cfg(windows)]
fn enumerate_processes_once() -> anyhow::Result<Vec<WindowsProcessInfo>> {
    use anyhow::Context;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .context("创建 Windows 进程快照失败")?;
    let _guard = HandleGuard(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut processes = Vec::new();
    unsafe { Process32FirstW(snapshot, &mut entry) }.context("读取 Windows 进程快照首项失败")?;
    loop {
        let process_id = entry.th32ProcessID;
        let (executable_path, creation_time) = query_process_identity(process_id);
        processes.push(WindowsProcessInfo {
            process_id,
            parent_process_id: entry.th32ParentProcessID,
            exe_file: nul_terminated_wide_to_string(&entry.szExeFile),
            executable_path,
            creation_time,
        });
        if let Err(error) = unsafe { Process32NextW(snapshot, &mut entry) } {
            if error.code() == ERROR_NO_MORE_FILES.to_hresult() {
                return Ok(processes);
            }
            return Err(error).context("读取 Windows 进程快照后续项失败");
        }
    }
}

#[cfg(windows)]
pub fn process_is_running(process_id: u32) -> anyhow::Result<bool> {
    use anyhow::Context;
    // Only a missing PID or a signaled process handle proves exit. Access
    // denial and other query failures must remain unknown, never exited.
    let handle = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, process_id) } {
        Ok(handle) => handle,
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(false),
        Err(error) => return Err(error).context("打开 Windows 进程句柄失败"),
    };
    let _guard = HandleGuard(handle);
    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_TIMEOUT => Ok(true),
        WAIT_OBJECT_0 => Ok(false),
        WAIT_FAILED => Err(windows::core::Error::from_win32()).context("检测 Windows 进程状态失败"),
        status => anyhow::bail!("Windows 进程状态返回未知结果：{status:?}"),
    }
}

#[cfg(windows)]
pub fn terminate_process_if_matches(
    process_id: u32,
    expected_path: &Path,
    expected_creation_time: u64,
) -> bool {
    let Ok(handle) = (unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            process_id,
        )
    }) else {
        return false;
    };
    if handle.is_invalid() {
        return false;
    }
    let _guard = HandleGuard(handle);
    let Some(executable_path) = query_process_image_path_from_handle(handle) else {
        return false;
    };
    let Some(creation_time) = query_process_creation_time_from_handle(handle) else {
        return false;
    };
    if creation_time != expected_creation_time
        || !process_paths_equal(&executable_path, expected_path)
    {
        return false;
    }
    unsafe { TerminateProcess(handle, 0) }.is_ok()
}

#[cfg(windows)]
pub fn terminate_process_if_creation_matches(process_id: u32, expected_creation_time: u64) -> bool {
    let Ok(handle) = (unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            process_id,
        )
    }) else {
        return false;
    };
    if handle.is_invalid() {
        return false;
    }
    let _guard = HandleGuard(handle);
    if query_process_creation_time_from_handle(handle) != Some(expected_creation_time) {
        return false;
    }
    unsafe { TerminateProcess(handle, 0) }.is_ok()
}

#[cfg(windows)]
pub fn process_paths_equal(left: &Path, right: &Path) -> bool {
    normalize_process_path(left).eq_ignore_ascii_case(&normalize_process_path(right))
}

#[cfg(windows)]
pub fn activate_process_window(process_id: u32) -> bool {
    let Some(hwnd) = process_window(process_id, false) else {
        return false;
    };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        SetForegroundWindow(hwnd).as_bool()
    }
}

#[cfg(windows)]
pub fn apply_codey_icon_to_process_window(process_id: u32, icon_resource_path: PathBuf) -> bool {
    let Some(hwnd) = visible_window_for_process(process_id) else {
        return false;
    };
    let mut applied = false;
    if apply_window_icons(hwnd, &icon_resource_path) {
        applied = true;
    }
    if apply_taskbar_properties(hwnd, &icon_resource_path).is_ok() {
        applied = true;
    }
    applied
}

#[cfg(windows)]
fn query_process_identity(process_id: u32) -> (Option<PathBuf>, Option<u64>) {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) })
    else {
        return (None, None);
    };
    if handle.is_invalid() {
        return (None, None);
    }
    let _guard = HandleGuard(handle);
    (
        query_process_image_path_from_handle(handle),
        query_process_creation_time_from_handle(handle),
    )
}

#[cfg(windows)]
fn query_process_image_path_from_handle(handle: HANDLE) -> Option<PathBuf> {
    let mut buffer = vec![0u16; MAX_PATH as usize * 4];
    let mut len = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            Default::default(),
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
    }
    Some(PathBuf::from(OsString::from_wide(&buffer[..len as usize])))
}

#[cfg(windows)]
fn query_process_creation_time_from_handle(handle: HANDLE) -> Option<u64> {
    let mut creation_time = FILETIME::default();
    let mut exit_time = FILETIME::default();
    let mut kernel_time = FILETIME::default();
    let mut user_time = FILETIME::default();
    unsafe {
        GetProcessTimes(
            handle,
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        )
        .ok()?;
    }
    Some((u64::from(creation_time.dwHighDateTime) << 32) | u64::from(creation_time.dwLowDateTime))
}

#[cfg(windows)]
fn normalize_process_path(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('/', "\\");
    if let Some(path) = normalized.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{path}");
    }
    normalized
        .strip_prefix(r"\\?\")
        .unwrap_or(&normalized)
        .trim_end_matches('\\')
        .to_string()
}

#[cfg(windows)]
fn visible_window_for_process(process_id: u32) -> Option<HWND> {
    process_window(process_id, true)
}

#[cfg(windows)]
fn process_window(process_id: u32, visible_only: bool) -> Option<HWND> {
    let mut state = ActivateWindowState {
        process_id,
        hwnd: HWND::default(),
        visible_only,
        score: ProcessWindowScore::None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(find_process_window_proc),
            LPARAM((&mut state as *mut ActivateWindowState) as isize),
        );
    }
    if state.hwnd.is_invalid() {
        None
    } else {
        Some(state.hwnd)
    }
}

#[cfg(windows)]
struct ActivateWindowState {
    process_id: u32,
    hwnd: HWND,
    visible_only: bool,
    score: ProcessWindowScore,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProcessWindowScore {
    None,
    Fallback,
    Titled,
    AppWindow,
    TauriWindow,
}

#[cfg(windows)]
unsafe extern "system" fn find_process_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = unsafe { &mut *(lparam.0 as *mut ActivateWindowState) };
    if state.visible_only && !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    let mut window_process_id = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut window_process_id));
    }
    if window_process_id == state.process_id {
        let title_length = unsafe { GetWindowTextLengthW(hwnd) };
        let extended_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let mut class_name = [0u16; 256];
        let class_name_length = unsafe { GetClassNameW(hwnd, &mut class_name) }.max(0) as usize;
        let class_name = String::from_utf16_lossy(&class_name[..class_name_length]);
        let score = process_window_score(title_length > 0, extended_style, &class_name);
        if score > state.score {
            state.hwnd = hwnd;
            state.score = score;
        }
        if score == ProcessWindowScore::TauriWindow {
            return BOOL(0);
        }
    }
    BOOL(1)
}

#[cfg(windows)]
fn process_window_score(
    has_title: bool,
    extended_style: u32,
    class_name: &str,
) -> ProcessWindowScore {
    let is_app_window = extended_style & WS_EX_APPWINDOW.0 != 0;
    let is_tool_window = extended_style & WS_EX_TOOLWINDOW.0 != 0;
    if is_tool_window || is_auxiliary_window_class(class_name) {
        ProcessWindowScore::Fallback
    } else if class_name.eq_ignore_ascii_case("Tauri Window") {
        ProcessWindowScore::TauriWindow
    } else if is_app_window && !is_tool_window {
        ProcessWindowScore::AppWindow
    } else if has_title {
        ProcessWindowScore::Titled
    } else {
        ProcessWindowScore::Fallback
    }
}

#[cfg(windows)]
fn is_auxiliary_window_class(class_name: &str) -> bool {
    matches!(
        class_name.to_ascii_lowercase().as_str(),
        "ime" | "msctfime ui" | "tray_icon_app" | "tao thread event target"
    )
}

#[cfg(windows)]
fn apply_window_icons(hwnd: HWND, icon_resource_path: &Path) -> bool {
    let Some((large_icon, small_icon)) = load_cached_icons(icon_resource_path) else {
        return false;
    };
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETICON,
            WPARAM(ICON_BIG as usize),
            LPARAM(large_icon.0 as isize),
        );
        SendMessageW(
            hwnd,
            WM_SETICON,
            WPARAM(ICON_SMALL as usize),
            LPARAM(small_icon.0 as isize),
        );
    }
    true
}

#[cfg(windows)]
fn load_cached_icons(icon_resource_path: &Path) -> Option<(HICON, HICON)> {
    static ICONS: OnceLock<(usize, usize)> = OnceLock::new();
    let icons = ICONS.get_or_init(|| {
        let path = wide_null(icon_resource_path.as_os_str());
        let mut large_icon = HICON::default();
        let mut small_icon = HICON::default();
        let loaded = unsafe {
            ExtractIconExW(
                PCWSTR(path.as_ptr()),
                0,
                Some(&mut large_icon),
                Some(&mut small_icon),
                1,
            )
        };
        if loaded == 0 {
            (0, 0)
        } else {
            (large_icon.0 as usize, small_icon.0 as usize)
        }
    });
    if icons.0 == 0 || icons.1 == 0 {
        None
    } else {
        Some((
            HICON(icons.0 as *mut core::ffi::c_void),
            HICON(icons.1 as *mut core::ffi::c_void),
        ))
    }
}

#[cfg(windows)]
fn apply_taskbar_properties(hwnd: HWND, icon_resource_path: &Path) -> anyhow::Result<()> {
    use windows::Win32::Storage::EnhancedStorage::{
        PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
        PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
    };

    let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(hwnd)? };
    let icon_resource = format!("{},0", icon_resource_path.to_string_lossy());
    let relaunch_command = std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "codey.exe".to_string());
    set_property_string(&store, &PKEY_AppUserModel_ID, "com.codey.app.codex")?;
    set_property_string(
        &store,
        &PKEY_AppUserModel_RelaunchIconResource,
        &icon_resource,
    )?;
    set_property_string(
        &store,
        &PKEY_AppUserModel_RelaunchDisplayNameResource,
        "Codey",
    )?;
    set_property_string(
        &store,
        &PKEY_AppUserModel_RelaunchCommand,
        &relaunch_command,
    )?;
    unsafe {
        store.Commit()?;
    }
    Ok(())
}

#[cfg(windows)]
fn set_property_string(
    store: &IPropertyStore,
    key: &windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY,
    value: &str,
) -> anyhow::Result<()> {
    let variant = PROPVARIANT::from(value);
    unsafe {
        store.SetValue(key, &variant)?;
    }
    Ok(())
}

#[cfg(windows)]
fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(once(0)).collect()
}

#[cfg(windows)]
fn nul_terminated_wide_to_string(value: &[u16]) -> String {
    let len = value.iter().position(|ch| *ch == 0).unwrap_or(value.len());
    OsString::from_wide(&value[..len])
        .to_string_lossy()
        .to_string()
}

#[cfg(windows)]
struct HandleGuard(HANDLE);

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    // 【自动化测试】Windows 进程 - 原生检测区分存活与退出
    #[test]
    fn native_process_probe_reports_current_and_exited_processes() {
        assert!(process_is_running(std::process::id()).unwrap());
        assert!(!process_is_running(u32::MAX).unwrap());
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "exit", "0"])
            .spawn()
            .unwrap();
        child.wait().unwrap();
        assert!(!process_is_running(child.id()).unwrap());
        assert!(
            enumerate_processes()
                .unwrap()
                .iter()
                .any(|process| process.process_id == std::process::id())
        );
    }

    #[test]
    fn application_window_outranks_titled_ime_and_tool_windows() {
        let ime_score = process_window_score(true, 0, "IME");
        let tool_score = process_window_score(false, WS_EX_TOOLWINDOW.0, "Tao Thread Event Target");
        let app_score = process_window_score(true, WS_EX_APPWINDOW.0, "Chrome_WidgetWin_1");
        let tauri_score = process_window_score(true, 0, "Tauri Window");
        let auxiliary_app_score = process_window_score(true, WS_EX_APPWINDOW.0, "tray_icon_app");

        assert!(tauri_score > app_score);
        assert!(app_score > ime_score);
        assert_eq!(ime_score, tool_score);
        assert_eq!(auxiliary_app_score, ProcessWindowScore::Fallback);
    }

    #[test]
    fn process_path_comparison_handles_case_separators_and_extended_prefixes() {
        assert!(process_paths_equal(
            Path::new(r"\\?\C:\Program Files\Codey\codey.exe"),
            Path::new(r"c:/program files/codey/CODEY.EXE"),
        ));
        assert!(!process_paths_equal(
            Path::new(r"C:\Program Files\Codey\codey.exe"),
            Path::new(r"D:\Program Files\Codey\codey.exe"),
        ));
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    // 【自动化测试】Windows 进程 - 快照故障重试有界且不返回空成功
    #[test]
    fn snapshot_failure_is_retried_and_never_becomes_an_empty_success() {
        let mut attempts = 0;
        let snapshot = retry_process_snapshot(|| {
            attempts += 1;
            if attempts == 2 {
                Ok(vec![7])
            } else {
                anyhow::bail!("snapshot unavailable")
            }
        })
        .unwrap();
        assert_eq!(snapshot, vec![7]);
        assert_eq!(attempts, 2);

        let mut attempts = 0;
        let result: anyhow::Result<Vec<u32>> = retry_process_snapshot(|| {
            attempts += 1;
            anyhow::bail!("snapshot unavailable")
        });
        assert!(result.is_err());
        assert_eq!(attempts, 3);
    }
}
