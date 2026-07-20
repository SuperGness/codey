use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use codex_plus_core::settings::RelayProtocol;
use directories::BaseDirs;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use toml_edit::{DocumentMut, Item};

use crate::config::{CodeyConfig, ProviderProfile};

const APP_TYPE: &str = "codex";
const OFFICIAL_PROVIDER_ID: &str = "codex-official";
const LOCAL_OFFICIAL_PROVIDER_ID: &str = "local-official";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CurrentProvider {
    pub id: String,
    pub name: String,
    pub official: bool,
    pub base_url: String,
    pub protocol: RelayProtocol,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchStatus {
    pub available: bool,
    pub path: String,
    pub changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub provider: CurrentProvider,
}

#[derive(Debug, Clone)]
struct ProviderRecord {
    id: String,
    name: String,
    settings_config: String,
    category: Option<String>,
}

#[derive(Debug, Clone)]
struct ProviderConnection {
    base_url: String,
    api_key: String,
    protocol: RelayProtocol,
    official: bool,
}

pub fn default_db_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CC_SWITCH_DB_PATH") {
        return PathBuf::from(path);
    }
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".cc-switch/cc-switch.db"))
        .unwrap_or_else(|| PathBuf::from(".cc-switch/cc-switch.db"))
}

pub fn sync_current_provider(
    config: &CodeyConfig,
    codex_home: &Path,
) -> Result<(CodeyConfig, CcSwitchStatus)> {
    sync_current_provider_from_paths(config, &default_db_path(), codex_home)
}

fn sync_current_provider_from_paths(
    config: &CodeyConfig,
    db_path: &Path,
    codex_home: &Path,
) -> Result<(CodeyConfig, CcSwitchStatus)> {
    let (profile, provider, available, message) = if db_path.is_file() {
        let record = read_current_provider(db_path)?.ok_or_else(|| {
            anyhow::anyhow!("cc-switch 没有选中的 Codex 配置，请先在 cc-switch 中选择线路")
        })?;
        let connection = provider_connection(&record)?;
        let provider = CurrentProvider {
            id: record.id.clone(),
            name: record.name.clone(),
            official: connection.official,
            base_url: connection.base_url.clone(),
            protocol: connection.protocol,
            source: "cc-switch".to_string(),
        };
        let profile = profile_from_provider(&provider, connection.api_key, true);
        (profile, provider, true, None)
    } else {
        let (provider, api_key) = local_provider(codex_home)?;
        let profile = profile_from_provider(&provider, api_key, false);
        (
            profile,
            provider,
            false,
            Some("未检测到 cc-switch，已读取本地 Codex 直登配置".to_string()),
        )
    };

    let mut next = config.clone();
    next.active_profile_id = profile.id.clone();
    next.profiles = vec![profile];
    next = next.normalize();
    let changed = &next != config;
    let status = CcSwitchStatus {
        available,
        path: db_path.to_string_lossy().to_string(),
        changed,
        message,
        provider,
    };
    Ok((next, status))
}

pub fn status_from_config(config: &CodeyConfig) -> CcSwitchStatus {
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == config.active_profile_id)
        .or_else(|| config.profiles.first());
    let available = profile
        .and_then(|profile| profile.cc_switch_provider_id.as_ref())
        .is_some();
    let provider = profile
        .map(|profile| CurrentProvider {
            id: profile
                .cc_switch_provider_id
                .clone()
                .unwrap_or_else(|| profile.id.clone()),
            name: profile.name.clone(),
            official: profile.cc_switch_read_only,
            base_url: profile.base_url.clone(),
            protocol: profile.protocol,
            source: if available { "cc-switch" } else { "local" }.to_string(),
        })
        .unwrap_or_else(|| CurrentProvider {
            id: LOCAL_OFFICIAL_PROVIDER_ID.to_string(),
            name: "OpenAI 官方直登".to_string(),
            official: true,
            base_url: String::new(),
            protocol: RelayProtocol::Responses,
            source: "local".to_string(),
        });
    CcSwitchStatus {
        available,
        path: default_db_path().to_string_lossy().to_string(),
        changed: false,
        message: (!available).then(|| "当前使用本地 Codex 直登配置".to_string()),
        provider,
    }
}

fn profile_from_provider(
    provider: &CurrentProvider,
    api_key: String,
    cc_switch_managed: bool,
) -> ProviderProfile {
    ProviderProfile {
        id: provider.id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        api_key,
        protocol: provider.protocol,
        cc_switch_provider_id: cc_switch_managed.then(|| provider.id.clone()),
        cc_switch_read_only: provider.official,
    }
}

fn read_current_provider(path: &Path) -> Result<Option<ProviderRecord>> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("打开 cc-switch 数据库失败：{}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection
        .query_row(
            "SELECT id, name, settings_config, category
             FROM providers
             WHERE app_type=?1 AND is_current=1
             ORDER BY CASE WHEN sort_index IS NULL THEN 1 ELSE 0 END, sort_index, created_at, name
             LIMIT 1",
            params![APP_TYPE],
            |row| {
                Ok(ProviderRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    settings_config: row.get(2)?,
                    category: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn provider_connection(record: &ProviderRecord) -> Result<ProviderConnection> {
    let settings = serde_json::from_str::<Value>(&record.settings_config)
        .context("解析 cc-switch 当前线路失败")?;
    let config_text = settings
        .get("config")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let document = DocumentMut::from_str(config_text).unwrap_or_default();
    let provider_key = document
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or_default();
    let provider_table = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_key))
        .and_then(Item::as_table_like);
    let base_url = provider_table
        .and_then(|table| table.get("base_url"))
        .and_then(Item::as_str)
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    let wire_api = provider_table
        .and_then(|table| table.get("wire_api"))
        .and_then(Item::as_str)
        .unwrap_or("responses");
    let auth = settings.get("auth").and_then(Value::as_object);
    let api_key = auth
        .and_then(|auth| auth.get("OPENAI_API_KEY"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let auth_mode = auth
        .and_then(|auth| auth.get("auth_mode"))
        .and_then(Value::as_str);
    let official = record.id == OFFICIAL_PROVIDER_ID
        || record.category.as_deref() == Some("official")
        || auth_mode == Some("chatgpt")
        || provider_key.is_empty()
        || base_url.is_empty();
    if !official && !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        bail!("cc-switch 当前第三方线路缺少有效的 API 地址");
    }
    Ok(ProviderConnection {
        base_url,
        api_key: if official { String::new() } else { api_key },
        protocol: protocol_from_wire_api(wire_api),
        official,
    })
}

fn local_provider(codex_home: &Path) -> Result<(CurrentProvider, String)> {
    let config_path = codex_home.join("config.toml");
    let config = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取本地 Codex 配置失败：{}", config_path.display()));
        }
    };
    let document = DocumentMut::from_str(&config).unwrap_or_default();
    let provider_id = document
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(LOCAL_OFFICIAL_PROVIDER_ID);
    let table = document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table_like);
    let mut base_url = table
        .and_then(|provider| provider.get("base_url"))
        .and_then(Item::as_str)
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    let name = table
        .and_then(|provider| provider.get("name"))
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(provider_id);
    let wire_api = table
        .and_then(|provider| provider.get("wire_api"))
        .and_then(Item::as_str)
        .unwrap_or("responses");
    let auth = fs::read(codex_home.join("auth.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let auth_mode = auth
        .as_ref()
        .and_then(|auth| auth.get("auth_mode"))
        .and_then(Value::as_str);
    let api_key = auth
        .as_ref()
        .and_then(|auth| auth.get("OPENAI_API_KEY"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let official_endpoint = base_url.is_empty()
        || base_url.contains("chatgpt.com/backend-api/codex")
        || base_url.contains("api.openai.com");
    let official = official_endpoint && (auth_mode == Some("chatgpt") || api_key.is_empty());
    if !official && base_url.is_empty() {
        base_url = "https://api.openai.com/v1".to_string();
    }
    let provider = CurrentProvider {
        id: if official && provider_id == LOCAL_OFFICIAL_PROVIDER_ID {
            LOCAL_OFFICIAL_PROVIDER_ID.to_string()
        } else {
            provider_id.to_string()
        },
        name: if official {
            "OpenAI 官方直登".to_string()
        } else if name == LOCAL_OFFICIAL_PROVIDER_ID {
            "OpenAI API".to_string()
        } else {
            name.to_string()
        },
        official,
        base_url,
        protocol: protocol_from_wire_api(wire_api),
        source: "local".to_string(),
    };
    Ok((provider, if official { String::new() } else { api_key }))
}

fn protocol_from_wire_api(value: &str) -> RelayProtocol {
    if value.to_ascii_lowercase().contains("chat") {
        RelayProtocol::ChatCompletions
    } else {
        RelayProtocol::Responses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cc-switch.db");
        let home = directory.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE providers (
                    id TEXT NOT NULL,
                    app_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    settings_config TEXT NOT NULL,
                    category TEXT,
                    created_at INTEGER,
                    sort_index INTEGER,
                    is_current BOOLEAN NOT NULL DEFAULT 0,
                    PRIMARY KEY (id, app_type)
                );",
            )
            .unwrap();
        (directory, path, home)
    }

    fn insert_provider(path: &Path, id: &str, name: &str, url: &str, current: bool) {
        let settings = json!({
            "auth": {"OPENAI_API_KEY": format!("{id}-secret")},
            "config": format!(
                "model_provider = \"custom\"\n\n[model_providers.custom]\nname = \"custom\"\nbase_url = \"{url}\"\nwire_api = \"responses\"\n"
            )
        });
        Connection::open(path)
            .unwrap()
            .execute(
                "INSERT INTO providers
                 (id, app_type, name, settings_config, sort_index, is_current)
                 VALUES (?1, 'codex', ?2, ?3, 0, ?4)",
                params![id, name, settings.to_string(), current],
            )
            .unwrap();
    }

    #[test]
    fn imports_only_the_current_cc_switch_provider() {
        let (_directory, path, home) = fixture();
        insert_provider(&path, "route-a", "线路 A", "https://a.example/v1", false);
        insert_provider(&path, "route-b", "线路 B", "https://b.example/v1", true);

        let (synced, status) =
            sync_current_provider_from_paths(&CodeyConfig::default(), &path, &home).unwrap();

        assert!(status.available);
        assert_eq!(status.provider.id, "route-b");
        assert_eq!(synced.profiles.len(), 1);
        assert_eq!(synced.profiles[0].base_url, "https://b.example/v1");
        assert_eq!(synced.profiles[0].api_key, "route-b-secret");
    }

    #[test]
    fn official_tokens_are_never_copied_into_a_provider_profile() {
        let (_directory, path, home) = fixture();
        let settings = json!({
            "auth": {"auth_mode": "chatgpt", "tokens": {"access_token": "secret"}},
            "config": ""
        });
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO providers
                 (id, app_type, name, settings_config, category, sort_index, is_current)
                 VALUES ('codex-official', 'codex', 'OpenAI Official', ?1, 'official', 0, 1)",
                [settings.to_string()],
            )
            .unwrap();

        let (synced, status) =
            sync_current_provider_from_paths(&CodeyConfig::default(), &path, &home).unwrap();

        assert!(status.provider.official);
        assert!(synced.profiles[0].api_key.is_empty());
    }

    #[test]
    fn falls_back_to_local_official_login_without_cc_switch() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("auth.json"),
            br#"{"auth_mode":"chatgpt","tokens":{"access_token":"secret"}}"#,
        )
        .unwrap();

        let (synced, status) = sync_current_provider_from_paths(
            &CodeyConfig::default(),
            &directory.path().join("missing.db"),
            &home,
        )
        .unwrap();

        assert!(!status.available);
        assert!(status.provider.official);
        assert_eq!(status.provider.source, "local");
        assert!(synced.profiles[0].api_key.is_empty());
    }

    #[test]
    fn model_selections_survive_provider_synchronization() {
        let (_directory, path, home) = fixture();
        insert_provider(&path, "route-a", "线路 A", "https://a.example/v1", true);
        let mut config = CodeyConfig::default();
        config
            .selected_models_by_provider
            .insert("route-a".into(), vec!["custom-model".into()]);

        let (synced, _) = sync_current_provider_from_paths(&config, &path, &home).unwrap();

        assert_eq!(synced.selected_models(), &["custom-model"]);
    }
}
