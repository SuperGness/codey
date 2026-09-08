use std::net::TcpListener;

pub fn select_packaged_codex_debug_port(requested: u16) -> u16 {
    select_packaged_codex_debug_port_with(
        requested,
        cfg!(windows),
        can_bind_loopback_port,
        find_available_loopback_port,
    )
}

pub fn select_packaged_codex_debug_port_with(
    requested: u16,
    is_windows: bool,
    can_bind: impl Fn(u16) -> bool,
    find_available: impl Fn() -> u16,
) -> u16 {
    if !is_windows || can_bind(requested) {
        requested
    } else {
        find_available()
    }
}

pub fn can_bind_loopback_port(port: u16) -> bool {
    if port == 0 {
        return true;
    }
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub fn find_available_loopback_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_windows_keeps_requested_debug_port_even_when_busy() {
        assert_eq!(
            select_packaged_codex_debug_port_with(9229, false, |_| false, || 1),
            9229
        );
    }

    #[test]
    fn windows_falls_back_to_an_available_port_when_requested_is_busy() {
        assert_eq!(
            select_packaged_codex_debug_port_with(9229, true, |_| false, || 4321),
            4321
        );
        assert_eq!(
            select_packaged_codex_debug_port_with(9229, true, |_| true, || 4321),
            9229
        );
    }
}
