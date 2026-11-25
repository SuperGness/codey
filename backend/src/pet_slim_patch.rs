use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::value::{RawValue, to_raw_value};

const GLOBAL_STATE_SQLITE_FILE: &str = ".codex-global-state.sqlite";
const GLOBAL_STATE_SQLITE_WAL_FILE: &str = ".codex-global-state.sqlite-wal";
const GLOBAL_STATE_FILE: &str = ".codex-global-state.json";
const GLOBAL_SCOPE: &str = "global";
const PET_OPEN_KEY: &str = "electron-avatar-overlay-open";
/// Codex 设置注册表里的当前宠物，与其他设置一同存放在全局状态中。
const SELECTED_AVATAR_KEY: &str = "selected-avatar-id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateFileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalStateStamp {
    sqlite: Option<StateFileStamp>,
    sqlite_wal: Option<StateFileStamp>,
    primary: Option<StateFileStamp>,
    backup: Option<StateFileStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PetSlimReport {
    pub slim_enabled: bool,
    pub changed: bool,
    pub state_path: PathBuf,
}

pub fn configure(codex_home: &Path, slim_enabled: bool) -> Result<PetSlimReport> {
    fs::create_dir_all(codex_home)
        .with_context(|| format!("创建 Codex 目录失败：{}", codex_home.display()))?;
    let sqlite_path = codex_home.join(GLOBAL_STATE_SQLITE_FILE);
    if sqlite_path.is_file() {
        return configure_sqlite(&sqlite_path, slim_enabled);
    }
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

/// 当前选中的 Codex 宠物 id；从未选择过时为 None。
pub fn selected_avatar_id(codex_home: &Path) -> Result<Option<String>> {
    let sqlite_path = codex_home.join(GLOBAL_STATE_SQLITE_FILE);
    if sqlite_path.is_file() {
        return selected_avatar_id_sqlite(&sqlite_path);
    }
    let state = read_state(
        &codex_home.join(GLOBAL_STATE_FILE),
        &codex_home.join(format!("{GLOBAL_STATE_FILE}.bak")),
    )?;
    Ok(state
        .get(SELECTED_AVATAR_KEY)
        .and_then(|value| serde_json::from_str::<Option<String>>(value.get()).ok())
        .flatten()
        .filter(|id| !id.is_empty()))
}

/// 全局状态数据库与 JSON 文件的元数据摘要，用于判断选中宠物缓存是否有效。
pub fn global_state_stamp(codex_home: &Path) -> GlobalStateStamp {
    GlobalStateStamp {
        sqlite: file_stamp(&codex_home.join(GLOBAL_STATE_SQLITE_FILE)),
        sqlite_wal: file_stamp(&codex_home.join(GLOBAL_STATE_SQLITE_WAL_FILE)),
        primary: file_stamp(&codex_home.join(GLOBAL_STATE_FILE)),
        backup: file_stamp(&codex_home.join(format!("{GLOBAL_STATE_FILE}.bak"))),
    }
}

fn configure_sqlite(sqlite_path: &Path, slim_enabled: bool) -> Result<PetSlimReport> {
    let connection = open_global_state_sqlite(sqlite_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    ensure_settings_table(&connection, sqlite_path)?;
    let desired_open_state = !slim_enabled;
    let current = read_sqlite_setting(&connection, PET_OPEN_KEY)?
        .and_then(|raw| serde_json::from_str::<bool>(&raw).ok());
    let changed = current != Some(desired_open_state);
    if changed {
        let encoded = if desired_open_state { "true" } else { "false" };
        connection
            .execute(
                "INSERT INTO settings (scope, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value
                 WHERE settings.value IS NOT excluded.value",
                params![GLOBAL_SCOPE, PET_OPEN_KEY, encoded],
            )
            .with_context(|| format!("更新 Codex 全局状态数据库失败：{}", sqlite_path.display()))?;
    }
    Ok(PetSlimReport {
        slim_enabled,
        changed,
        state_path: sqlite_path.to_path_buf(),
    })
}

fn selected_avatar_id_sqlite(sqlite_path: &Path) -> Result<Option<String>> {
    let connection = open_global_state_sqlite(sqlite_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    ensure_settings_table(&connection, sqlite_path)?;
    let raw = read_sqlite_setting(&connection, SELECTED_AVATAR_KEY)?;
    Ok(raw
        .and_then(|value| serde_json::from_str::<Option<String>>(&value).ok())
        .flatten()
        .filter(|id| !id.is_empty()))
}

fn open_global_state_sqlite(path: &Path, flags: OpenFlags) -> Result<Connection> {
    let connection = Connection::open_with_flags(path, flags)
        .with_context(|| format!("打开 Codex 全局状态数据库失败：{}", path.display()))?;
    connection
        .busy_timeout(Duration::from_secs(3))
        .with_context(|| format!("配置 Codex 全局状态数据库超时失败：{}", path.display()))?;
    Ok(connection)
}

fn ensure_settings_table(connection: &Connection, path: &Path) -> Result<()> {
    let has_table: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'settings'",
            [],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("检查 Codex 全局状态数据库表结构失败：{}", path.display()))?;
    if has_table.is_none() {
        bail!("Codex 全局状态数据库缺少 settings 表：{}", path.display());
    }
    Ok(())
}

fn read_sqlite_setting(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM settings WHERE scope = ?1 AND key = ?2",
            params![GLOBAL_SCOPE, key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| format!("读取 Codex 全局状态键值失败：{key}"))
}

fn file_stamp(path: &Path) -> Option<StateFileStamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(StateFileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
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
    fn reads_the_selected_avatar_from_global_state() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(selected_avatar_id(temp.path()).unwrap(), None);
        let path = temp.path().join(GLOBAL_STATE_FILE);
        fs::write(&path, r#"{"selected-avatar-id":"hoots","other":1}"#).unwrap();
        assert_eq!(
            selected_avatar_id(temp.path()).unwrap().as_deref(),
            Some("hoots")
        );
        fs::write(&path, r#"{"selected-avatar-id":null}"#).unwrap();
        assert_eq!(selected_avatar_id(temp.path()).unwrap(), None);
    }

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

    fn create_global_state_sqlite(home: &Path, rows: &[(&str, &str, &str)]) -> PathBuf {
        let path = home.join(GLOBAL_STATE_SQLITE_FILE);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (
                scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
                PRIMARY KEY (scope, key)
            ) WITHOUT ROWID;
            PRAGMA user_version = 1;",
        )
        .unwrap();
        for (scope, key, value) in rows {
            conn.execute(
                "INSERT INTO settings (scope, key, value) VALUES (?1, ?2, ?3)",
                params![scope, key, value],
            )
            .unwrap();
        }
        path
    }

    #[test]
    fn configures_and_reads_sqlite_global_state_preserving_unrelated_rows() {
        let temp = tempfile::tempdir().unwrap();
        let sqlite_path = create_global_state_sqlite(
            temp.path(),
            &[
                ("global", "selected-avatar-id", r#""hoots""#),
                ("global", "electron-avatar-overlay-open", "true"),
                ("global", "keep-key", r#"{"nested":1}"#),
                ("atom", "other-key", "42"),
            ],
        );

        assert_eq!(
            selected_avatar_id(temp.path()).unwrap().as_deref(),
            Some("hoots")
        );

        let report = configure(temp.path(), true).unwrap();
        assert!(report.slim_enabled);
        assert!(report.changed);
        assert_eq!(report.state_path, sqlite_path);
        assert!(!configure(temp.path(), true).unwrap().changed);

        let conn = Connection::open(&sqlite_path).unwrap();
        assert_eq!(
            read_sqlite_setting(&conn, PET_OPEN_KEY).unwrap().as_deref(),
            Some("false")
        );
        assert_eq!(
            read_sqlite_setting(&conn, "keep-key").unwrap().as_deref(),
            Some(r#"{"nested":1}"#)
        );
        let atom_val: String = conn
            .query_row(
                "SELECT value FROM settings WHERE scope = 'atom' AND key = 'other-key'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(atom_val, "42");

        let restored = configure(temp.path(), false).unwrap();
        assert!(!restored.slim_enabled);
        assert!(restored.changed);
        assert_eq!(
            read_sqlite_setting(&conn, PET_OPEN_KEY).unwrap().as_deref(),
            Some("true")
        );
    }

    #[test]
    fn sqlite_global_state_overrides_legacy_json_and_updates_stamp() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(GLOBAL_STATE_FILE),
            r#"{"selected-avatar-id":"legacy-pet"}"#,
        )
        .unwrap();
        let before = global_state_stamp(temp.path());

        create_global_state_sqlite(temp.path(), &[("global", "selected-avatar-id", r#""dewey""#)]);
        let after = global_state_stamp(temp.path());

        assert_ne!(before, after);
        assert_eq!(
            selected_avatar_id(temp.path()).unwrap().as_deref(),
            Some("dewey")
        );
    }

    #[test]
    fn refuses_sqlite_without_settings_table() {
        let temp = tempfile::tempdir().unwrap();
        let sqlite_path = temp.path().join(GLOBAL_STATE_SQLITE_FILE);
        let conn = Connection::open(&sqlite_path).unwrap();
        conn.execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
            .unwrap();
        drop(conn);

        let err = configure(temp.path(), true).unwrap_err();
        assert!(err.to_string().contains("缺少 settings 表"));
    }
}
