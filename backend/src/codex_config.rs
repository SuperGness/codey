use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use codex_plus_core::settings::RelayProtocol;
use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::config::{ProviderProfile, default_config_path};
use crate::provider_lease::CODEY_PROVIDER_ID;

pub const GLOBAL_PROVIDER_ID: &str = "codey_global";
pub const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const RESERVED_PROVIDER_IDS: [&str; 6] = [
    "amazon-bedrock",
    "openai",
    "ollama",
    "lmstudio",
    "oss",
    "ollama-chat",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeConfigLease {
    backup_dir: PathBuf,
    original_config_exists: bool,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    applied_base_url: Option<String>,
}

pub fn codex_home() -> PathBuf {
    codex_plus_core::relay_config::default_codex_home_dir()
}

pub fn lease_marker_path() -> PathBuf {
    default_config_path()
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join("codex-lease.json")
}

pub fn apply_runtime_provider_config(
    home: &Path,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
) -> Result<PathBuf> {
    let marker = lease_marker_path();
    let backup_root = marker
        .parent()
        .unwrap_or_else(|| Path::new(".codey"))
        .join("codex-backups");
    apply_runtime_provider_config_at(
        home,
        profile,
        provider_id,
        use_official_catalog,
        &marker,
        &backup_root,
    )
}

fn apply_runtime_provider_config_at(
    home: &Path,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
    marker: &Path,
    backup_root: &Path,
) -> Result<PathBuf> {
    fs::create_dir_all(home)?;
    let config_path = home.join("config.toml");
    let original_config = read_optional(&config_path)?;
    let backup_dir = backup_root.join(format!("{}-{}", timestamp_millis(), std::process::id()));
    fs::create_dir_all(&backup_dir)?;
    if let Some(bytes) = original_config.as_deref() {
        fs::write(backup_dir.join("config.toml"), bytes)?;
    }

    let existing = String::from_utf8(original_config.clone().unwrap_or_default())
        .context("Codex config.toml 不是 UTF-8")?;
    let provider_id = normalized_provider_id(provider_id);
    let updated = patch_config(&existing, profile, &provider_id, use_official_catalog)?;
    let applied_base_url = provider_base_url(&updated, &provider_id);
    let state = RuntimeConfigLease {
        backup_dir: backup_dir.clone(),
        original_config_exists: original_config.is_some(),
        provider_id: Some(provider_id),
        applied_base_url,
    };
    if let Err(error) = write_lease(marker, &state) {
        let _ = fs::remove_dir_all(&backup_dir);
        return Err(error);
    }

    if let Err(write_error) = atomic_write(&config_path, updated.as_bytes()) {
        let rollback = match original_config.as_deref() {
            Some(bytes) => atomic_write(&config_path, bytes),
            None => remove_optional(&config_path),
        };
        if let Err(rollback_error) = rollback {
            anyhow::bail!(
                "写入 Codey 临时 Codex 配置失败：{write_error}；回滚原配置也失败：{rollback_error}"
            );
        }
        let _ = remove_optional(marker);
        let _ = fs::remove_dir_all(&backup_dir);
        return Err(write_error);
    }
    Ok(backup_dir)
}

fn write_lease(path: &Path, state: &RuntimeConfigLease) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(path, &serde_json::to_vec_pretty(state)?)
}

pub fn restore_runtime_provider_config(home: &Path) -> Result<bool> {
    restore_runtime_provider_config_at(home, &lease_marker_path())
}

fn restore_runtime_provider_config_at(home: &Path, marker: &Path) -> Result<bool> {
    let state = match fs::read_to_string(marker) {
        Ok(contents) => serde_json::from_str::<RuntimeConfigLease>(&contents)
            .with_context(|| format!("解析 Codey Codex lease 失败：{}", marker.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let config_path = home.join("config.toml");
    let current = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 Codex 配置失败：{}", config_path.display()));
        }
    };
    let provider_id = state.provider_id.as_deref().unwrap_or(CODEY_PROVIDER_ID);
    let provider_matches =
        root_key_string(&current, "model_provider").as_deref() == Some(provider_id);
    let endpoint_matches = state.applied_base_url.as_deref().is_none_or(|base_url| {
        provider_base_url(&current, provider_id).as_deref() == Some(base_url)
    });
    if !provider_matches || !endpoint_matches {
        remove_optional(marker)?;
        return Ok(false);
    }

    let backup_config = state.backup_dir.join("config.toml");
    if state.original_config_exists {
        if !backup_config.exists() {
            anyhow::bail!("找不到 Codex 原配置备份：{}", backup_config.display());
        }
        atomic_write(&config_path, &fs::read(&backup_config)?)?;
    } else {
        remove_optional(&config_path)?;
    }
    remove_optional(marker)?;
    Ok(true)
}

/// Installs a stable non-reserved provider for the official account flow.
/// Direct third-party profiles temporarily reuse this provider id while Codey
/// runs, then the exact original configuration is restored.
pub fn ensure_global_model_provider(home: &Path) -> Result<String> {
    fs::create_dir_all(home)?;
    let config_path = home.join("config.toml");
    let original = read_optional(&config_path)?;
    let existing = String::from_utf8(original.clone().unwrap_or_default())
        .context("Codex config.toml 不是 UTF-8")?;
    let mut doc = parse_document(&existing)?;

    if let Some(providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        for provider in RESERVED_PROVIDER_IDS {
            providers.remove(provider);
        }
    }
    let current_provider = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(ToString::to_string);
    if let Some(provider) = current_provider.as_deref()
        && !is_reserved_provider(provider)
        && provider != CODEY_PROVIDER_ID
        && provider != GLOBAL_PROVIDER_ID
    {
        write_global_provider_migration_if_changed(home, &config_path, &existing, &doc, original)?;
        return Ok(provider.to_string());
    }

    ensure_provider_table(&mut doc)?;
    doc["model_providers"]
        .as_table_mut()
        .expect("model_providers was initialized")[GLOBAL_PROVIDER_ID] =
        Item::Table(official_provider_table());
    doc["model_provider"] = value(GLOBAL_PROVIDER_ID);
    write_global_provider_migration_if_changed(home, &config_path, &existing, &doc, original)?;
    Ok(GLOBAL_PROVIDER_ID.to_string())
}

pub fn patch_config(
    existing: &str,
    profile: &ProviderProfile,
    provider_id: &str,
    use_official_catalog: bool,
) -> Result<String> {
    let mut doc = parse_document(existing)?;
    ensure_provider_table(&mut doc)?;
    let provider_id = normalized_provider_id(provider_id);
    let provider = if profile.cc_switch_read_only {
        official_provider_table()
    } else {
        direct_provider_table(profile)?
    };
    doc["model_providers"]
        .as_table_mut()
        .expect("model_providers was initialized")[&provider_id] = Item::Table(provider);
    doc["model_provider"] = value(provider_id);
    if use_official_catalog {
        doc["model_catalog_json"] = value(crate::model_catalog::relative_path());
    } else {
        doc.as_table_mut().remove("model_catalog_json");
    }
    cap_desktop_reasoning_efforts(&mut doc)?;
    remove_model_selection(&mut doc);
    document_string(&doc)
}

fn direct_provider_table(profile: &ProviderProfile) -> Result<Table> {
    let base_url = profile.normalized_base_url();
    if base_url.is_empty() {
        anyhow::bail!("第三方线路缺少 API 地址");
    }
    let mut provider = Table::new();
    provider["name"] = value(profile.name.trim());
    provider["base_url"] = value(base_url);
    provider["wire_api"] = value(match profile.protocol {
        RelayProtocol::Responses => "responses",
        RelayProtocol::ChatCompletions => "chat",
    });
    provider["requires_openai_auth"] = value(true);
    if !profile.api_key.trim().is_empty() {
        provider["experimental_bearer_token"] = value(profile.api_key.trim());
    }
    Ok(provider)
}

fn official_provider_table() -> Table {
    let mut provider = Table::new();
    provider["name"] = value("OpenAI (Codey Global)");
    provider["base_url"] = value(CHATGPT_CODEX_BASE_URL);
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(true);
    provider
}

fn parse_document(existing: &str) -> Result<DocumentMut> {
    if existing.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        existing
            .parse::<DocumentMut>()
            .context("Codex config.toml TOML 解析失败")
    }
}

fn ensure_provider_table(doc: &mut DocumentMut) -> Result<()> {
    if doc
        .get("model_providers")
        .and_then(Item::as_table)
        .is_none()
    {
        doc["model_providers"] = Item::Table(Table::new());
    }
    doc["model_providers"]
        .as_table_mut()
        .map(|_| ())
        .ok_or_else(|| anyhow::anyhow!("model_providers 必须是 TOML table"))
}

fn write_global_provider_migration_if_changed(
    home: &Path,
    config_path: &Path,
    existing: &str,
    doc: &DocumentMut,
    original: Option<Vec<u8>>,
) -> Result<()> {
    let updated = document_string(doc)?;
    if updated != existing {
        backup_global_provider_migration(home, original.as_deref())?;
        atomic_write(config_path, updated.as_bytes())?;
    }
    Ok(())
}

fn document_string(doc: &DocumentMut) -> Result<String> {
    let mut result = doc.to_string();
    if !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

fn cap_desktop_reasoning_efforts(doc: &mut DocumentMut) -> Result<()> {
    if doc.get("desktop").and_then(Item::as_table).is_none() {
        doc["desktop"] = Item::Table(Table::new());
    }
    let desktop = doc["desktop"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("desktop 必须是 TOML table"))?;
    let mut efforts = Array::new();
    for effort in ["low", "medium", "high", "xhigh"] {
        efforts.push(effort);
    }
    desktop["enabled-reasoning-efforts"] = value(efforts);
    Ok(())
}

fn remove_model_selection(doc: &mut DocumentMut) {
    doc.as_table_mut().remove("model");
    let Some(active_profile) = doc
        .get("profile")
        .and_then(Item::as_str)
        .map(ToString::to_string)
    else {
        return;
    };
    let Some(profiles) = doc.get_mut("profiles").and_then(Item::as_table_mut) else {
        return;
    };
    if let Some(profile) = profiles
        .get_mut(&active_profile)
        .and_then(Item::as_table_mut)
    {
        profile.remove("model");
    }
}

fn root_key_string(contents: &str, key: &str) -> Option<String> {
    let doc = contents.parse::<DocumentMut>().ok()?;
    doc.get(key).and_then(Item::as_str).map(ToString::to_string)
}

fn provider_base_url(contents: &str, provider_id: &str) -> Option<String> {
    let doc = contents.parse::<DocumentMut>().ok()?;
    doc.get("model_providers")
        .and_then(Item::as_table)?
        .get(provider_id)
        .and_then(Item::as_table)?
        .get("base_url")
        .and_then(Item::as_str)
        .map(|value| value.trim_end_matches('/').to_string())
}

fn normalized_provider_id(provider_id: &str) -> String {
    let provider_id = provider_id.trim();
    if provider_id.is_empty()
        || provider_id == CODEY_PROVIDER_ID
        || is_reserved_provider(provider_id)
    {
        GLOBAL_PROVIDER_ID.to_string()
    } else {
        provider_id.to_string()
    }
}

fn is_reserved_provider(provider_id: &str) -> bool {
    RESERVED_PROVIDER_IDS.contains(&provider_id)
}

fn backup_global_provider_migration(home: &Path, original: Option<&[u8]>) -> Result<()> {
    let Some(original) = original else {
        return Ok(());
    };
    let backup_root = home.join("backups_state/codey-global-provider");
    let backup_dir = backup_root.join(format!("{}-{}", timestamp_millis(), std::process::id()));
    fs::create_dir_all(&backup_dir)?;
    fs::write(backup_dir.join("config.toml"), original)?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("路径没有父目录：{}", path.display()))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.codey-tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&temp, bytes)?;
    match fs::rename(&temp, path) {
        Ok(()) => {}
        Err(error) => {
            #[cfg(windows)]
            {
                if path.exists() {
                    fs::remove_file(path)?;
                    fs::rename(&temp, path)?;
                } else {
                    return Err(error.into());
                }
            }
            #[cfg(not(windows))]
            return Err(error.into());
        }
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("读取文件失败：{}", path.display())),
    }
}

fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("删除文件失败：{}", path.display())),
    }
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official_profile() -> ProviderProfile {
        let mut profile = ProviderProfile::new("OpenAI Official");
        profile.id = "codex-official".to_string();
        profile.cc_switch_read_only = true;
        profile
    }

    fn direct_profile(protocol: RelayProtocol) -> ProviderProfile {
        let mut profile = ProviderProfile::new("Relay");
        profile.base_url = "https://relay.example/v1".to_string();
        profile.api_key = "sk-direct".to_string();
        profile.protocol = protocol;
        profile
    }

    #[test]
    fn official_patch_uses_the_official_endpoint_and_catalog() {
        let result = patch_config(
            "model = \"gpt\"\nmodel_catalog_json = \"old.json\"\n",
            &official_profile(),
            GLOBAL_PROVIDER_ID,
            true,
        )
        .unwrap();
        assert!(result.contains("base_url = \"https://chatgpt.com/backend-api/codex\""));
        assert!(!result.contains("experimental_bearer_token"));
        assert_eq!(
            root_key_string(&result, "model_catalog_json").as_deref(),
            Some("model-catalogs/codey-official.json")
        );
        assert_eq!(root_key_string(&result, "model"), None);
    }

    #[test]
    fn direct_patch_configures_the_provider_without_a_loopback_endpoint() {
        let result = patch_config(
            "model_provider = \"openai\"\n",
            &direct_profile(RelayProtocol::ChatCompletions),
            "openai",
            false,
        )
        .unwrap();
        assert!(result.contains("base_url = \"https://relay.example/v1\""));
        assert!(result.contains("wire_api = \"chat\""));
        assert!(result.contains("experimental_bearer_token = \"sk-direct\""));
        assert!(!result.contains("127.0.0.1"));
        assert_eq!(
            root_key_string(&result, "model_provider").as_deref(),
            Some(GLOBAL_PROVIDER_ID)
        );
    }

    #[test]
    fn lease_restores_the_exact_original_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        let marker = temp.path().join("codey/codex-lease.json");
        let backup_root = temp.path().join("codey/codex-backups");
        fs::create_dir_all(&home).unwrap();
        let original = b"model_provider = \"codey_global\"\n\n[model_providers.codey_global]\nbase_url = \"https://chatgpt.com/backend-api/codex\"\n";
        fs::write(home.join("config.toml"), original).unwrap();

        apply_runtime_provider_config_at(
            &home,
            &direct_profile(RelayProtocol::Responses),
            GLOBAL_PROVIDER_ID,
            true,
            &marker,
            &backup_root,
        )
        .unwrap();
        let temporary = fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(
            provider_base_url(&temporary, GLOBAL_PROVIDER_ID).as_deref(),
            Some("https://relay.example/v1")
        );
        assert!(restore_runtime_provider_config_at(&home, &marker).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), original);
        assert!(!marker.exists());
    }

    #[test]
    fn installs_a_non_reserved_global_provider_for_builtin_openai() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model_provider = \"openai\"\nmodel = \"gpt-5\"\n",
        )
        .unwrap();
        assert_eq!(
            ensure_global_model_provider(&home).unwrap(),
            GLOBAL_PROVIDER_ID
        );
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert_eq!(
            provider_base_url(&config, GLOBAL_PROVIDER_ID).as_deref(),
            Some(CHATGPT_CODEX_BASE_URL)
        );
        assert!(!config.contains("[model_providers.openai]"));
    }

    #[test]
    fn preserves_an_existing_non_reserved_provider() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("codex-home");
        fs::create_dir_all(&home).unwrap();
        let original = "model_provider = \"company\"\n\n[model_providers.company]\nname = \"Company\"\nbase_url = \"https://example.com/v1\"\n";
        fs::write(home.join("config.toml"), original).unwrap();
        assert_eq!(
            ensure_global_model_provider(&home).unwrap(),
            "company".to_string()
        );
        assert_eq!(
            fs::read_to_string(home.join("config.toml")).unwrap(),
            original
        );
    }
}
