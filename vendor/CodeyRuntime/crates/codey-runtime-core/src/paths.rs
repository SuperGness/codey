use std::ffi::OsString;
use std::path::PathBuf;

const APP_STATE_DIR: &str = ".codex-session-delete";
const APP_STATE_DIR_ENV: &str = "CODEY_APP_STATE_DIR";
const DIAGNOSTIC_LOG_FILE: &str = "codey.log";

pub fn default_app_state_dir() -> PathBuf {
    if let Some(path) = app_state_dir_from_env_value(std::env::var_os(APP_STATE_DIR_ENV)) {
        return path;
    }

    if let Some(home_dir) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
        return home_dir.join(APP_STATE_DIR);
    }

    PathBuf::from(APP_STATE_DIR)
}

fn app_state_dir_from_env_value(value: Option<OsString>) -> Option<PathBuf> {
    value
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

pub fn default_diagnostic_log_path() -> PathBuf {
    default_app_state_dir().join(DIAGNOSTIC_LOG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_dir_env_override_accepts_non_empty_paths() {
        let state_dir = PathBuf::from("/tmp/codey-state");

        assert_eq!(
            app_state_dir_from_env_value(Some(state_dir.clone().into_os_string())),
            Some(state_dir)
        );
        assert_eq!(app_state_dir_from_env_value(Some(OsString::new())), None);
    }

    #[test]
    fn default_diagnostic_log_path_uses_app_state_directory() {
        let path = default_diagnostic_log_path();

        assert!(path.ends_with(".codex-session-delete/codey.log"));
    }
}
