use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::config::default_config_path;

pub const CODEY_PROVIDER_ID: &str = "codey_local";
const LEGACY_LEASE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRepairReport {
    pub databases: usize,
    pub restored_threads: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyProviderLeaseState {
    version: u32,
    fallback_provider: String,
    databases: Vec<LegacyDatabaseLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyDatabaseLease {
    path: PathBuf,
    threads: Vec<LegacyThreadProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyThreadProvider {
    id: String,
    provider: String,
}

fn legacy_marker_path() -> PathBuf {
    default_config_path()
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join("session-provider-lease-v1.json")
}

/// Restores a provider lease left by an older Codey build before migration to
/// the stable global provider strategy.
pub fn restore_legacy() -> Result<ProviderRepairReport> {
    restore_legacy_at(&legacy_marker_path())
}

fn restore_legacy_at(marker: &Path) -> Result<ProviderRepairReport> {
    let bytes = match fs::read(marker) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProviderRepairReport::default());
        }
        Err(error) => return Err(error.into()),
    };
    let state: LegacyProviderLeaseState = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析旧版会话 provider 租约失败：{}", marker.display()))?;
    if state.version != LEGACY_LEASE_VERSION {
        anyhow::bail!("不支持的会话 provider 租约版本：{}", state.version);
    }

    let mut report = ProviderRepairReport::default();
    for database in &state.databases {
        if !database.path.exists() {
            continue;
        }
        let mut connection = Connection::open(&database.path)
            .with_context(|| format!("打开 Codex 会话数据库失败：{}", database.path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        if !has_provider_column(&connection)? {
            continue;
        }
        report.databases += 1;
        let transaction = connection.transaction()?;
        report.restored_threads += transaction.execute(
            "UPDATE threads SET model_provider=?1 WHERE model_provider=?2",
            params![
                normalized_provider(&state.fallback_provider),
                CODEY_PROVIDER_ID
            ],
        )?;
        for thread in &database.threads {
            transaction.execute(
                "UPDATE threads SET model_provider=?1 WHERE id=?2",
                params![thread.provider, thread.id],
            )?;
        }
        transaction.commit()?;
    }
    remove_optional(marker)?;
    Ok(report)
}

fn has_provider_column(connection: &Connection) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM pragma_table_info('threads') WHERE name='model_provider' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false))
}

fn normalized_provider(provider: &str) -> String {
    let provider = provider.trim();
    if provider.is_empty() || provider == CODEY_PROVIDER_ID {
        "openai".to_string()
    } else {
        provider.to_string()
    }
}

fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("清理旧版会话租约失败：{}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_database(home: &Path) -> PathBuf {
        fs::create_dir_all(home).unwrap();
        let path = home.join("state_5.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL);\
                 INSERT INTO threads VALUES ('openai-thread', 'openai');\
                 INSERT INTO threads VALUES ('custom-thread', 'custom');\
                 INSERT INTO threads VALUES ('codey-thread', 'codey_local');",
            )
            .unwrap();
        path
    }

    fn provider(path: &Path, id: &str) -> String {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT model_provider FROM threads WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn restores_a_legacy_provider_lease_before_migration() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let marker = temp.path().join("lease.json");
        let path = create_database(&home);
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE threads SET model_provider=?1", [CODEY_PROVIDER_ID])
            .unwrap();
        let state = LegacyProviderLeaseState {
            version: LEGACY_LEASE_VERSION,
            fallback_provider: "openai".to_string(),
            databases: vec![LegacyDatabaseLease {
                path: path.clone(),
                threads: vec![
                    LegacyThreadProvider {
                        id: "openai-thread".to_string(),
                        provider: "openai".to_string(),
                    },
                    LegacyThreadProvider {
                        id: "custom-thread".to_string(),
                        provider: "custom".to_string(),
                    },
                ],
            }],
        };
        fs::write(&marker, serde_json::to_vec(&state).unwrap()).unwrap();

        let restored = restore_legacy_at(&marker).unwrap();
        assert_eq!(restored.restored_threads, 3);
        assert_eq!(provider(&path, "openai-thread"), "openai");
        assert_eq!(provider(&path, "custom-thread"), "custom");
        assert_eq!(provider(&path, "codey-thread"), "openai");
        assert!(!marker.exists());
    }
}
