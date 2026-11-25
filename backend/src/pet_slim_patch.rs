use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

const GLOBAL_STATE_SQLITE_FILE: &str = ".codex-global-state.sqlite";
const GLOBAL_STATE_SQLITE_WAL_FILE: &str = ".codex-global-state.sqlite-wal";
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
    if !sqlite_path.is_file() {
        // 全新 CODEX_HOME 尚未生成全局状态库时按无变更放行，由 Codex 首次启动自行建库。
        return Ok(PetSlimReport {
            slim_enabled,
            changed: false,
            state_path: sqlite_path,
        });
    }
    configure_sqlite(&sqlite_path, slim_enabled)
}

/// 当前选中的 Codex 宠物 id；从未选择过或状态库尚不存在时为 None。
pub fn selected_avatar_id(codex_home: &Path) -> Result<Option<String>> {
    let sqlite_path = codex_home.join(GLOBAL_STATE_SQLITE_FILE);
    if !sqlite_path.is_file() {
        return Ok(None);
    }
    selected_avatar_id_sqlite(&sqlite_path)
}

/// 全局状态数据库的元数据摘要，用于判断选中宠物缓存是否有效。
pub fn global_state_stamp(codex_home: &Path) -> GlobalStateStamp {
    GlobalStateStamp {
        sqlite: file_stamp(&codex_home.join(GLOBAL_STATE_SQLITE_FILE)),
        sqlite_wal: file_stamp(&codex_home.join(GLOBAL_STATE_SQLITE_WAL_FILE)),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn missing_sqlite_on_fresh_codex_home_is_clean_noop() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("fresh-home");
        let before = global_state_stamp(&home);

        let report = configure(&home, true).unwrap();
        assert!(report.slim_enabled);
        assert!(!report.changed);
        assert_eq!(report.state_path, home.join(GLOBAL_STATE_SQLITE_FILE));
        assert!(!report.state_path.exists());
        assert_eq!(selected_avatar_id(&home).unwrap(), None);

        create_global_state_sqlite(&home, &[("global", "selected-avatar-id", r#""dewey""#)]);
        let after = global_state_stamp(&home);
        assert_ne!(before, after);
        assert_eq!(selected_avatar_id(&home).unwrap().as_deref(), Some("dewey"));
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
