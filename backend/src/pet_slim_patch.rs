use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

const GLOBAL_STATE_FILE: &str = ".codex-global-state.json";
const PET_OPEN_KEY: &str = "electron-avatar-overlay-open";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PetSlimReport {
    pub slim_enabled: bool,
    pub changed: bool,
    pub state_path: PathBuf,
}

pub fn configure(codex_home: &Path, slim_enabled: bool) -> Result<PetSlimReport> {
    fs::create_dir_all(codex_home)
        .with_context(|| format!("创建 Codex 目录失败：{}", codex_home.display()))?;
    let state_path = codex_home.join(GLOBAL_STATE_FILE);
    let backup_path = codex_home.join(format!("{GLOBAL_STATE_FILE}.bak"));
    let mut state = read_state(&state_path, &backup_path)?;
    let desired_open_state = !slim_enabled;
    let changed = state.get(PET_OPEN_KEY).and_then(Value::as_bool) != Some(desired_open_state);

    if changed || !state_path.exists() {
        state.insert(PET_OPEN_KEY.to_string(), Value::Bool(desired_open_state));
        let bytes = serde_json::to_vec(&Value::Object(state))?;
        atomic_write(&state_path, &bytes)?;
        atomic_write(&backup_path, &bytes)?;
    }

    Ok(PetSlimReport {
        slim_enabled,
        changed,
        state_path,
    })
}

fn read_state(primary: &Path, backup: &Path) -> Result<Map<String, Value>> {
    match read_state_file(primary) {
        Ok(Some(state)) => Ok(state),
        Ok(None) => read_state_file(backup).map(|state| state.unwrap_or_default()),
        Err(primary_error) => match read_state_file(backup) {
            Ok(Some(state)) => Ok(state),
            Ok(None) | Err(_) => Err(primary_error),
        },
    }
}

fn read_state_file(path: &Path) -> Result<Option<Map<String, Value>>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 Codex 全局状态失败：{}", path.display()));
        }
    };
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Codex 全局状态失败：{}", path.display()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Codex 全局状态不是 JSON 对象：{}", path.display()))
        .map(Some)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codex 全局状态路径没有父目录"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Codex 全局状态路径没有文件名"))?
        .to_string_lossy();
    let temp = parent.join(format!(".{file_name}.codey-{}.tmp", std::process::id()));
    fs::write(&temp, bytes)
        .with_context(|| format!("写入 Codex 宠物精简临时状态失败：{}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
    }
    replace_file(&temp, path)
        .with_context(|| format!("更新 Codex 宠物状态失败：{}", path.display()))
}

fn replace_file(temp: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(temp, destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            #[cfg(windows)]
            if destination.exists() {
                fs::remove_file(destination)?;
                return fs::rename(temp, destination);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_a_closed_pet_without_losing_other_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(GLOBAL_STATE_FILE);
        fs::write(
            &path,
            br#"{"keep":"value","electron-avatar-overlay-open":true}"#,
        )
        .unwrap();

        let report = configure(temp.path(), true).unwrap();
        let state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

        assert!(report.slim_enabled);
        assert!(report.changed);
        assert_eq!(state["keep"], "value");
        assert_eq!(state[PET_OPEN_KEY], false);
        assert_eq!(
            fs::read(path).unwrap(),
            fs::read(temp.path().join(format!("{GLOBAL_STATE_FILE}.bak"))).unwrap()
        );
    }

    #[test]
    fn disabling_slim_mode_restores_the_pet_on_the_next_launch() {
        let temp = tempfile::tempdir().unwrap();
        configure(temp.path(), true).unwrap();

        let report = configure(temp.path(), false).unwrap();
        let state: Value = serde_json::from_slice(&fs::read(report.state_path).unwrap()).unwrap();

        assert!(!report.slim_enabled);
        assert!(report.changed);
        assert_eq!(state[PET_OPEN_KEY], true);
    }

    #[test]
    fn recovers_other_state_from_the_codex_backup() {
        let temp = tempfile::tempdir().unwrap();
        let backup = temp.path().join(format!("{GLOBAL_STATE_FILE}.bak"));
        fs::write(&backup, br#"{"fromBackup":42}"#).unwrap();

        configure(temp.path(), true).unwrap();
        let state: Value =
            serde_json::from_slice(&fs::read(temp.path().join(GLOBAL_STATE_FILE)).unwrap())
                .unwrap();

        assert_eq!(state["fromBackup"], 42);
        assert_eq!(state[PET_OPEN_KEY], false);
    }

    #[test]
    fn refuses_to_replace_a_corrupt_state_when_no_backup_is_available() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(GLOBAL_STATE_FILE), b"{broken").unwrap();

        let error = configure(temp.path(), true).unwrap_err();

        assert!(error.to_string().contains("解析 Codex 全局状态失败"));
    }
}
