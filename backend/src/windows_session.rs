use anyhow::{Result, bail, ensure};

const SHELL_LINK_HEADER_SIZE: u32 = 0x0000_004C;
const SHELL_LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];
const SLDF_RUNAS_USER: u32 = 0x0000_2000;
pub(crate) const DEELEVATED_RELAUNCH_ENV: &str = "CODEY_DEELEVATED_RELAUNCH";

/// Removes `RUNASADMIN` / `ELEVATECREATEPROCESS` from a Windows `AppCompatFlags\Layers`
/// value. Returns `None` when no elevation layer is present, `Some(None)` when the
/// entire registry value should be deleted, or `Some(Some(updated))` when other
/// non-elevation compatibility flags remain.
pub(crate) fn strip_runasadmin_layer(value: &str) -> Option<Option<String>> {
    let tokens = value.split_whitespace().collect::<Vec<_>>();
    let has_admin = tokens.iter().any(|token| {
        token.eq_ignore_ascii_case("RUNASADMIN")
            || token.eq_ignore_ascii_case("ELEVATECREATEPROCESS")
    });
    if !has_admin {
        return None;
    }
    let remaining = tokens
        .into_iter()
        .filter(|token| {
            !token.eq_ignore_ascii_case("RUNASADMIN")
                && !token.eq_ignore_ascii_case("ELEVATECREATEPROCESS")
        })
        .collect::<Vec<_>>();
    let meaningful = remaining.iter().any(|token| !matches!(*token, "~" | "!"));
    if !meaningful {
        Some(None)
    } else {
        Some(Some(remaining.join(" ")))
    }
}

/// Clears the `SLDF_RUNAS_USER` ("Run as administrator") flag in a Windows `.lnk`
/// `ShellLinkHeader`. Returns `true` when the flag was set and has been cleared.
pub(crate) fn clear_shortcut_runas_flag(bytes: &mut [u8]) -> bool {
    if bytes.len() < SHELL_LINK_HEADER_SIZE as usize {
        return false;
    }
    let header_size = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if header_size != SHELL_LINK_HEADER_SIZE || bytes[4..20] != SHELL_LINK_CLSID {
        return false;
    }
    let flags = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if (flags & SLDF_RUNAS_USER) == 0 {
        return false;
    }
    let cleared = (flags & !SLDF_RUNAS_USER).to_le_bytes();
    bytes[20..24].copy_from_slice(&cleared);
    true
}

pub(crate) fn normalize_windows_appcompat_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string()
}

pub(crate) fn build_deelevated_environment_block<I>(vars: I) -> Vec<u16>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut entries = Vec::new();
    for (key, value) in vars {
        if key.is_empty()
            || key.contains(['=', '\0'])
            || value.contains('\0')
            || key.eq_ignore_ascii_case(DEELEVATED_RELAUNCH_ENV)
        {
            continue;
        }
        if key.eq_ignore_ascii_case("__COMPAT_LAYER") {
            match strip_runasadmin_layer(&value) {
                None => entries.push(format!("{key}={value}")),
                Some(Some(cleaned)) => entries.push(format!("{key}={cleaned}")),
                Some(None) => {}
            }
            continue;
        }
        entries.push(format!("{key}={value}"));
    }
    entries.push(format!("{DEELEVATED_RELAUNCH_ENV}=1"));
    entries.sort_by_key(|entry| entry.to_ascii_uppercase());
    let mut block = Vec::new();
    for entry in entries {
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InformationClass {
    ConnectionState,
    ExtendedSessionInfo,
}

fn validate_buffer(
    actual_class: InformationClass,
    expected_class: InformationClass,
    is_null: bool,
    bytes: u32,
    expected_bytes: usize,
) -> Result<()> {
    ensure!(
        actual_class == expected_class,
        "unexpected WTS information class"
    );
    ensure!(!is_null, "WTS query returned a null buffer");
    ensure!(
        bytes as usize == expected_bytes,
        "unexpected WTS buffer size: {bytes}, expected {expected_bytes}"
    );
    Ok(())
}

fn connection_is_active(state: i32) -> Result<bool> {
    // WTS_CONNECTSTATE_CLASS is 0 (active) through 9 (initializing).
    match state {
        0 => Ok(true),
        1..=9 => Ok(false),
        _ => bail!("unknown WTS connection state: {state}"),
    }
}

fn validate_info_level(level: u32) -> Result<()> {
    ensure!(
        level == 1,
        "unsupported WTS session information level: {level}"
    );
    Ok(())
}

fn session_info_is_interactive(
    expected_session_id: u32,
    session_id: u32,
    connection_state: i32,
    session_flags: i32,
) -> Result<bool> {
    ensure!(
        session_id == expected_session_id,
        "WTS query returned session {session_id}, expected {expected_session_id}"
    );
    let active = connection_is_active(connection_state)?;
    // Windows 10/11: WTS_SESSIONSTATE_LOCK = 0, WTS_SESSIONSTATE_UNLOCK = 1.
    let unlocked = match session_flags {
        0 => false,
        1 => true,
        _ => bail!("unknown WTS session flags: {session_flags}"),
    };
    Ok(active && unlocked)
}

#[cfg(windows)]
pub(crate) fn current_session_is_interactive() -> Result<bool> {
    native::current_session_is_interactive()
}

#[cfg(windows)]
pub(crate) fn clear_admin_launch_settings() {
    native::clear_admin_launch_settings()
}

#[cfg(windows)]
pub(crate) fn relaunch_deelevated_if_needed() -> bool {
    native::relaunch_deelevated_if_needed()
}

#[cfg(windows)]
mod native {
    use super::*;
    use anyhow::Context;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, HANDLE,
    };
    use windows::Win32::Security::{
        DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, TOKEN_ADJUST_DEFAULT,
        TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_ELEVATION,
        TOKEN_QUERY, TokenElevation, TokenPrimary,
    };
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTS_CONNECTSTATE_CLASS, WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS,
        WTSConnectState, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
    };
    use windows::Win32::System::Threading::{
        CREATE_PROCESS_LOGON_FLAGS, CREATE_UNICODE_ENVIRONMENT, CreateProcessWithTokenW,
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_INFORMATION,
        PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
    use windows::core::{PCWSTR, PWSTR};

    const APPCOMPAT_LAYERS_SUBKEY: &str =
        r"Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers";

    struct WtsBuffer {
        information_class: InformationClass,
        pointer: PWSTR,
        bytes: u32,
    }

    impl WtsBuffer {
        fn query(session_id: u32, information_class: InformationClass) -> Result<Self> {
            let native_class: WTS_INFO_CLASS = match information_class {
                InformationClass::ConnectionState => WTSConnectState,
                InformationClass::ExtendedSessionInfo => WTSSessionInfoEx,
            };
            let mut buffer = Self {
                information_class,
                pointer: PWSTR::null(),
                bytes: 0,
            };
            // Keep ownership even on failure in case the API assigned a buffer.
            unsafe {
                WTSQuerySessionInformationW(
                    WTS_CURRENT_SERVER_HANDLE,
                    session_id,
                    native_class,
                    &mut buffer.pointer,
                    &mut buffer.bytes,
                )
            }
            .with_context(|| format!("query WTS {information_class:?} for session {session_id}"))?;
            Ok(buffer)
        }

        fn connection_state(&self) -> Result<WTS_CONNECTSTATE_CLASS> {
            validate_buffer(
                self.information_class,
                InformationClass::ConnectionState,
                self.pointer.is_null(),
                self.bytes,
                size_of::<WTS_CONNECTSTATE_CLASS>(),
            )?;
            // The requested class and exact size identify a structure of integer fields.
            // Reading without an alignment requirement avoids borrowing the API buffer.
            Ok(unsafe {
                self.pointer
                    .0
                    .cast::<WTS_CONNECTSTATE_CLASS>()
                    .read_unaligned()
            })
        }

        fn extended_session_info(&self) -> Result<WTSINFOEXW> {
            validate_buffer(
                self.information_class,
                InformationClass::ExtendedSessionInfo,
                self.pointer.is_null(),
                self.bytes,
                size_of::<WTSINFOEXW>(),
            )?;
            // All fields, including the union member, contain only integer values.
            Ok(unsafe { self.pointer.0.cast::<WTSINFOEXW>().read_unaligned() })
        }
    }

    impl Drop for WtsBuffer {
        fn drop(&mut self) {
            if !self.pointer.is_null() {
                unsafe { WTSFreeMemory(self.pointer.0.cast()) };
            }
        }
    }

    pub(super) fn current_session_is_interactive() -> Result<bool> {
        let mut session_id = 0;
        // Resolve this process's session, including RDP sessions, rather than the console.
        unsafe { ProcessIdToSessionId(std::process::id(), &mut session_id) }
            .context("resolve the current process's Windows session")?;

        let state =
            WtsBuffer::query(session_id, InformationClass::ConnectionState)?.connection_state()?;
        if !connection_is_active(state.0)? {
            return Ok(false);
        }

        let info = WtsBuffer::query(session_id, InformationClass::ExtendedSessionInfo)?
            .extended_session_info()?;
        validate_info_level(info.Level)?;
        // Level 1 is the only supported union member; inspect it after validating Level.
        let info = unsafe { info.Data.WTSInfoExLevel1 };
        session_info_is_interactive(
            session_id,
            info.SessionId,
            info.SessionState.0,
            info.SessionFlags,
        )
    }

    fn encode_wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn clear_registry_runasadmin_layer(hive: HKEY, executable_path: &str) {
        let subkey = encode_wide_null(APPCOMPAT_LAYERS_SUBKEY);
        let value_name = encode_wide_null(executable_path);
        let mut buffer = vec![0u16; 4096];
        let mut size = (buffer.len() * size_of::<u16>()) as u32;
        let status = unsafe {
            RegGetValueW(
                hive,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value_name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status == ERROR_FILE_NOT_FOUND
            || status == ERROR_PATH_NOT_FOUND
            || status != ERROR_SUCCESS
        {
            return;
        }
        let end = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        let Ok(current) = String::from_utf16(&buffer[..end]) else {
            return;
        };
        let Some(updated) = strip_runasadmin_layer(&current) else {
            return;
        };
        let mut key = HKEY::default();
        let open_status =
            unsafe { RegOpenKeyExW(hive, PCWSTR(subkey.as_ptr()), 0, KEY_SET_VALUE, &mut key) };
        if open_status != ERROR_SUCCESS {
            return;
        }
        match updated {
            None => {
                let _ = unsafe { RegDeleteValueW(key, PCWSTR(value_name.as_ptr())) };
            }
            Some(remaining) => {
                let wide = encode_wide_null(&remaining);
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        wide.as_ptr().cast::<u8>(),
                        wide.len() * size_of::<u16>(),
                    )
                };
                let _ = unsafe {
                    RegSetValueExW(key, PCWSTR(value_name.as_ptr()), 0, REG_SZ, Some(bytes))
                };
            }
        }
        let _ = unsafe { RegCloseKey(key) };
    }

    fn clear_shortcut_file_runas_flag(path: &Path) {
        if !path.is_file() {
            return;
        }
        let Ok(mut bytes) = crate::fs_util::read_bounded(path, 256 * 1024) else {
            return;
        };
        if clear_shortcut_runas_flag(&mut bytes) {
            let _ = crate::fs_util::atomic_write_preserving_permissions(path, &bytes);
        }
    }

    pub(super) fn clear_admin_launch_settings() {
        let mut executables = Vec::new();
        if let Ok(current) = std::env::current_exe() {
            if let Ok(canonical) = std::fs::canonicalize(&current) {
                if let Some(parent) = canonical.parent() {
                    executables.push(normalize_windows_appcompat_path(
                        &parent.join("codey-fastctx.exe"),
                    ));
                }
                executables.push(normalize_windows_appcompat_path(&canonical));
            }
            if let Some(parent) = current.parent() {
                let sibling = normalize_windows_appcompat_path(&parent.join("codey-fastctx.exe"));
                if !executables.contains(&sibling) {
                    executables.push(sibling);
                }
            }
            let normalized = normalize_windows_appcompat_path(&current);
            if !executables.contains(&normalized) {
                executables.push(normalized);
            }
        }
        for path in &executables {
            for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
                clear_registry_runasadmin_layer(hive, path);
            }
        }

        let mut shortcuts = Vec::new();
        if let Some(user_dirs) = directories::UserDirs::new()
            && let Some(desktop) = user_dirs.desktop_dir()
        {
            shortcuts.push(desktop.join("Codey.lnk"));
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            let path = PathBuf::from(profile).join("Desktop").join("Codey.lnk");
            if !shortcuts.contains(&path) {
                shortcuts.push(path);
            }
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            shortcuts.push(
                PathBuf::from(appdata)
                    .join(r"Microsoft\Windows\Start Menu\Programs\Codey\Codey.lnk"),
            );
        }
        if let Some(program_data) = std::env::var_os("PROGRAMDATA") {
            shortcuts.push(
                PathBuf::from(program_data)
                    .join(r"Microsoft\Windows\Start Menu\Programs\Codey\Codey.lnk"),
            );
        }
        for shortcut in &shortcuts {
            clear_shortcut_file_runas_flag(shortcut);
        }
    }

    fn token_is_elevated(token: HANDLE) -> Result<bool> {
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
        }
        .context("查询 Windows 进程令牌提权状态失败")?;
        Ok(elevation.TokenIsElevated != 0)
    }

    fn process_is_elevated(process: HANDLE) -> Result<bool> {
        let mut raw_token = HANDLE::default();
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw_token) }
            .context("打开 Windows 进程令牌失败")?;
        let token = unsafe { OwnedHandle::from_raw_handle(raw_token.0) };
        token_is_elevated(HANDLE(token.as_raw_handle()))
    }

    fn quote_windows_command_arg(arg: &str) -> String {
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

    pub(super) fn relaunch_deelevated_if_needed() -> bool {
        if std::env::var_os(DEELEVATED_RELAUNCH_ENV).is_some() {
            return false;
        }
        let Ok(true) = process_is_elevated(unsafe { GetCurrentProcess() }) else {
            return false;
        };
        match relaunch_via_shell_token() {
            Ok(true) => true,
            Ok(false) => false,
            Err(error) => {
                let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                    "desktop.deelevate_relaunch_skipped",
                    serde_json::json!({
                        "processId": std::process::id(),
                        "detail": format!("{error:#}"),
                    }),
                );
                false
            }
        }
    }

    fn relaunch_via_shell_token() -> Result<bool> {
        let shell_window = unsafe { GetShellWindow() };
        if shell_window.0.is_null() {
            return Ok(false);
        }
        let mut shell_pid = 0u32;
        unsafe { GetWindowThreadProcessId(shell_window, Some(&mut shell_pid)) };
        if shell_pid == 0 || shell_pid == std::process::id() {
            return Ok(false);
        }
        let current_session = codey_runtime_core::windows_process_session_id(std::process::id());
        let shell_session = codey_runtime_core::windows_process_session_id(shell_pid);
        if current_session.is_none() || current_session != shell_session {
            return Ok(false);
        }
        let shell_process = unsafe {
            OpenProcess(PROCESS_QUERY_INFORMATION, false, shell_pid)
                .or_else(|_| OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, shell_pid))
        }
        .context("打开 Windows 桌面 Shell 进程失败")?;
        let shell_process = unsafe { OwnedHandle::from_raw_handle(shell_process.0) };
        let mut raw_shell_token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                HANDLE(shell_process.as_raw_handle()),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut raw_shell_token,
            )
        }
        .context("打开 Windows 桌面 Shell 令牌失败")?;
        let shell_token = unsafe { OwnedHandle::from_raw_handle(raw_shell_token.0) };
        if token_is_elevated(HANDLE(shell_token.as_raw_handle()))? {
            return Ok(false);
        }
        let mut raw_primary_token = HANDLE::default();
        unsafe {
            DuplicateTokenEx(
                HANDLE(shell_token.as_raw_handle()),
                TOKEN_ASSIGN_PRIMARY
                    | TOKEN_DUPLICATE
                    | TOKEN_QUERY
                    | TOKEN_ADJUST_DEFAULT
                    | TOKEN_ADJUST_SESSIONID,
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut raw_primary_token,
            )
        }
        .context("复制 Windows 桌面普通用户主令牌失败")?;
        let primary_token = unsafe { OwnedHandle::from_raw_handle(raw_primary_token.0) };

        let executable = std::env::current_exe().context("读取当前 Codey 路径失败")?;
        let executable_wide = executable
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let command_line = std::env::args_os()
            .map(|arg| quote_windows_command_arg(&arg.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command_line_wide = command_line
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let current_dir = std::env::current_dir()
            .ok()
            .or_else(|| executable.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let current_dir_wide = current_dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let environment_block =
            build_deelevated_environment_block(std::env::vars_os().filter_map(|(key, value)| {
                Some((key.into_string().ok()?, value.into_string().ok()?))
            }));
        let startup_info = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process_info = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessWithTokenW(
                HANDLE(primary_token.as_raw_handle()),
                CREATE_PROCESS_LOGON_FLAGS(0),
                PCWSTR(executable_wide.as_ptr()),
                PWSTR(command_line_wide.as_mut_ptr()),
                CREATE_UNICODE_ENVIRONMENT,
                Some(environment_block.as_ptr().cast()),
                PCWSTR(current_dir_wide.as_ptr()),
                &startup_info,
                &mut process_info,
            )
        }
        .context("以普通用户身份重启 Codey 失败")?;
        if !process_info.hThread.is_invalid() {
            let _ = unsafe { CloseHandle(process_info.hThread) };
        }
        if !process_info.hProcess.is_invalid() {
            let _ = unsafe { CloseHandle(process_info.hProcess) };
        }
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "desktop.relaunched_deelevated",
            serde_json::json!({
                "elevatedProcessId": std::process::id(),
                "deelevatedProcessId": process_info.dwProcessId,
            }),
        );
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_runasadmin_layer_deletes_or_preserves_other_flags() {
        assert_eq!(strip_runasadmin_layer(""), None);
        assert_eq!(strip_runasadmin_layer("~ HIGHDPIAWARE"), None);
        assert_eq!(strip_runasadmin_layer("~ RUNASADMIN"), Some(None));
        assert_eq!(strip_runasadmin_layer("runasadmin"), Some(None));
        assert_eq!(
            strip_runasadmin_layer("~ RunAsAdmin ElevateCreateProcess"),
            Some(None)
        );
        assert_eq!(
            strip_runasadmin_layer("~ HIGHDPIAWARE RUNASADMIN"),
            Some(Some("~ HIGHDPIAWARE".to_string()))
        );
        assert_eq!(
            strip_runasadmin_layer("RUNASADMIN WIN8RTM"),
            Some(Some("WIN8RTM".to_string()))
        );
    }

    #[test]
    fn clear_shortcut_runas_flag_updates_only_valid_elevated_lnk_headers() {
        let mut header = vec![0u8; 76];
        header[0..4].copy_from_slice(&SHELL_LINK_HEADER_SIZE.to_le_bytes());
        header[4..20].copy_from_slice(&SHELL_LINK_CLSID);
        let initial_flags = 0x0000_0081u32 | SLDF_RUNAS_USER;
        header[20..24].copy_from_slice(&initial_flags.to_le_bytes());

        assert!(clear_shortcut_runas_flag(&mut header));
        assert_eq!(
            u32::from_le_bytes([header[20], header[21], header[22], header[23]]),
            0x0000_0081
        );
        assert!(!clear_shortcut_runas_flag(&mut header));
        assert!(!clear_shortcut_runas_flag(&mut header[..20]));
    }

    #[test]
    fn deelevated_environment_block_strips_runasadmin_and_marks_relaunch() {
        let block = build_deelevated_environment_block([
            ("__COMPAT_LAYER".to_string(), "~ RUNASADMIN".to_string()),
            (
                "CODEX_HOME".to_string(),
                r"C:\Users\tester\.codex".to_string(),
            ),
        ]);
        let decoded = String::from_utf16(&block).unwrap();
        let entries = decoded
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            entries,
            vec![
                r"CODEX_HOME=C:\Users\tester\.codex",
                "CODEY_DEELEVATED_RELAUNCH=1",
            ]
        );
        assert_eq!(
            normalize_windows_appcompat_path(std::path::Path::new(
                r"\\?\C:\Users\tester\AppData\Local\Programs\Codey\Codey.exe"
            )),
            r"C:\Users\tester\AppData\Local\Programs\Codey\Codey.exe"
        );
    }

    #[test]
    fn only_active_and_unlocked_sessions_are_interactive() {
        assert!(session_info_is_interactive(7, 7, 0, 1).unwrap());
        assert!(!session_info_is_interactive(7, 7, 0, 0).unwrap());
        for state in 1..=9 {
            assert!(!connection_is_active(state).unwrap());
            assert!(!session_info_is_interactive(7, 7, state, 1).unwrap());
        }
        // WTSDisconnected is 4, including a disconnected RDP session.
        assert!(!session_info_is_interactive(7, 7, 4, 0).unwrap());
    }

    #[test]
    fn unknown_states_and_flags_remain_errors() {
        for state in [-1, 10, i32::MAX] {
            assert!(connection_is_active(state).is_err());
            assert!(session_info_is_interactive(7, 7, state, 1).is_err());
        }
        // -1 is WTS_SESSIONSTATE_UNKNOWN, not an unlocked session.
        for flags in [-1, 2, i32::MAX] {
            assert!(session_info_is_interactive(7, 7, 0, flags).is_err());
            assert!(session_info_is_interactive(7, 7, 4, flags).is_err());
        }
    }

    #[test]
    fn wrong_session_and_unsupported_level_are_errors() {
        assert!(session_info_is_interactive(7, 8, 0, 1).is_err());
        assert!(validate_info_level(1).is_ok());
        for level in [0, 2, u32::MAX] {
            assert!(validate_info_level(level).is_err());
        }
    }

    #[test]
    fn buffer_requires_matching_class_nonnull_pointer_and_exact_size() {
        let class = InformationClass::ConnectionState;
        assert!(validate_buffer(class, class, false, 4, 4).is_ok());
        assert!(validate_buffer(class, class, true, 4, 4).is_err());
        for bytes in [0, 1, 3, 5, u32::MAX] {
            assert!(validate_buffer(class, class, false, bytes, 4).is_err());
        }
        assert!(
            validate_buffer(class, InformationClass::ExtendedSessionInfo, false, 4, 4).is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_constants_match_the_testable_policy() {
        use windows::Win32::System::RemoteDesktop::{
            WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNKNOWN, WTS_SESSIONSTATE_UNLOCK, WTSActive,
            WTSDisconnected, WTSInit,
        };
        assert_eq!(WTSActive.0, 0);
        assert_eq!(WTSDisconnected.0, 4);
        assert_eq!(WTSInit.0, 9);
        assert_eq!(WTS_SESSIONSTATE_LOCK, 0);
        assert_eq!(WTS_SESSIONSTATE_UNLOCK, 1);
        assert_eq!(WTS_SESSIONSTATE_UNKNOWN as i32, -1);
    }
}
