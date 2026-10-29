use anyhow::{Result, bail, ensure};

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
mod native {
    use super::*;
    use anyhow::Context;
    use std::mem::size_of;
    use windows::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTS_CONNECTSTATE_CLASS, WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS,
        WTSConnectState, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
    };
    use windows::core::PWSTR;

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
