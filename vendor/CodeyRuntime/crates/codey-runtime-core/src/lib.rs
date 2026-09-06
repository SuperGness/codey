pub mod app_paths;
pub mod bridge;
pub mod cdp;
pub(crate) mod codex_home;
pub use codex_home::default_codex_home_dir;
pub mod codex_sqlite;
pub mod config_manager;
pub mod diagnostic_log;
pub mod launcher;
pub mod model_suffix;
pub mod models;
pub mod paths;
pub mod plugin_marketplace;
pub mod ports;
pub mod version;
#[cfg(windows)]
mod windows_integration;

#[cfg(windows)]
pub fn windows_create_no_window() -> u32 {
    windows_integration::CREATE_NO_WINDOW
}

#[cfg(windows)]
pub fn windows_open_url(url: &str) -> anyhow::Result<()> {
    windows_integration::open_url(url)
}

#[cfg(windows)]
pub fn windows_activate_process_window(process_id: u32) -> bool {
    windows_integration::activate_process_window(process_id)
}

#[cfg(windows)]
pub fn windows_apply_codey_icon_to_process_window(
    process_id: u32,
    icon_resource_path: std::path::PathBuf,
) -> bool {
    windows_integration::apply_codey_icon_to_process_window(process_id, icon_resource_path)
}

#[cfg(windows)]
pub fn windows_enumerate_processes() -> Vec<windows_integration::WindowsProcessInfo> {
    windows_integration::enumerate_processes()
}

#[cfg(windows)]
pub fn windows_process_paths_equal(left: &std::path::Path, right: &std::path::Path) -> bool {
    windows_integration::process_paths_equal(left, right)
}

#[cfg(windows)]
pub fn windows_terminate_process_if_matches(
    process_id: u32,
    expected_path: &std::path::Path,
    expected_creation_time: u64,
) -> bool {
    windows_integration::terminate_process_if_matches(
        process_id,
        expected_path,
        expected_creation_time,
    )
}

#[cfg(windows)]
pub fn windows_terminate_process_if_creation_matches(
    process_id: u32,
    expected_creation_time: u64,
) -> bool {
    windows_integration::terminate_process_if_creation_matches(process_id, expected_creation_time)
}
