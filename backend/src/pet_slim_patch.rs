use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::value::{RawValue, to_raw_value};

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
    let changed = state
        .get(PET_OPEN_KEY)
        .and_then(|value| serde_json::from_str::<bool>(value.get()).ok())
        != Some(desired_open_state);

    if changed || !state_path.exists() {
        state.insert(PET_OPEN_KEY.to_string(), to_raw_value(&desired_open_state)?);
        let bytes = serde_json::to_vec(&state)?;
        crate::fs_util::atomic_write_private(&state_path, &bytes)
            .with_context(|| format!("更新 Codex 宠物状态失败：{}", state_path.display()))?;
        crate::fs_util::atomic_write_private(&backup_path, &bytes)
            .with_context(|| format!("更新 Codex 宠物备份状态失败：{}", backup_path.display()))?;
    }

    Ok(PetSlimReport {
        slim_enabled,
        changed,
        state_path,
    })
}

fn read_state(primary: &Path, backup: &Path) -> Result<BTreeMap<String, Box<RawValue>>> {
    match read_state_file(primary) {
        Ok(Some(state)) => Ok(state),
        Ok(None) => read_state_file(backup).map(|state| state.unwrap_or_default()),
        Err(primary_error) => match read_state_file(backup) {
            Ok(Some(state)) => Ok(state),
            Ok(None) | Err(_) => Err(primary_error),
        },
    }
}

fn read_state_file(path: &Path) -> Result<Option<BTreeMap<String, Box<RawValue>>>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 Codex 全局状态失败：{}", path.display()));
        }
    };
    // Codex can store lone UTF-16 surrogates; preserve unrelated JSON values verbatim.
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Codex 全局状态失败：{}", path.display()))?;
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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
        for original in ["{broken", "[]", r#"{"\ud800":1}"#] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(GLOBAL_STATE_FILE);
            fs::write(&path, original).unwrap();

            let error = configure(temp.path(), true).unwrap_err();

            assert!(error.to_string().contains("解析 Codex 全局状态失败"));
            assert_eq!(fs::read_to_string(path).unwrap(), original);
            assert!(
                !temp
                    .path()
                    .join(format!("{GLOBAL_STATE_FILE}.bak"))
                    .exists()
            );
        }
    }

    #[test]
    fn preserves_raw_state_with_surrogates_in_primary_and_backup() {
        let original = r#"{"high":"\ud800","low":"\udfff","normal":"中文😀\ud83d\ude00","nested": { "\ud800": ["\udfff", {"keep":1.00e+2}] },"electron-avatar-overlay-open":true}"#;
        for from_backup in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let primary = temp.path().join(GLOBAL_STATE_FILE);
            let backup = temp.path().join(format!("{GLOBAL_STATE_FILE}.bak"));
            fs::write(if from_backup { &backup } else { &primary }, original).unwrap();
            if from_backup {
                fs::write(&primary, b"{broken").unwrap();
            }

            let before: BTreeMap<String, Box<RawValue>> = serde_json::from_str(original).unwrap();
            assert!(configure(temp.path(), true).unwrap().changed);
            let bytes = fs::read(&primary).unwrap();
            let after: BTreeMap<String, Box<RawValue>> = serde_json::from_slice(&bytes).unwrap();
            for (key, value) in before
                .iter()
                .filter(|(key, _)| key.as_str() != PET_OPEN_KEY)
            {
                assert_eq!(after[key].get(), value.get());
            }
            assert_eq!(after[PET_OPEN_KEY].get(), "false");
            assert_eq!(bytes, fs::read(&backup).unwrap());
            assert!(!configure(temp.path(), true).unwrap().changed);
            assert_eq!(bytes, fs::read(&primary).unwrap());
            assert!(configure(temp.path(), false).unwrap().changed);
        }
    }
}
