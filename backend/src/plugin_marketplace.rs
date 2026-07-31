use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use toml_edit::DocumentMut;

use crate::error_log;

/// Repairs the official/local marketplace registration without touching the
/// Codex installation directory. The core crate owns the platform-specific
/// config format and local remote marketplace; Codey only exposes a small,
/// renderer-friendly status/list API around it.
pub fn ensure_marketplaces(home: &Path) -> Result<Value> {
    // Official curated snapshots no longer always ship a sibling remote market.
    // Repair materializes it from the local curated tree before registration.
    let initialized_remote =
        codey_runtime_core::plugin_marketplace::materialize_openai_curated_remote_marketplace(
            home,
        )
        .context("物化官方远程插件市场失败")?;
    let remote =
        codey_runtime_core::plugin_marketplace::ensure_openai_curated_remote_marketplace_available(
            home,
        )
        .context("初始化官方远程插件市场失败")?;
    let curated_changed =
        codey_runtime_core::plugin_marketplace::ensure_openai_curated_marketplace_config(home)
            .context("注册官方插件市场失败")?;
    let role_changed =
        codey_runtime_core::plugin_marketplace::ensure_role_specific_plugins_marketplace_config(
            home,
        )
        .context("注册本地工具插件市场失败")?;
    let official = codey_runtime_core::plugin_marketplace::openai_curated_marketplace_status(home);
    let remote_status =
        codey_runtime_core::plugin_marketplace::openai_curated_remote_marketplace_status(home);
    Ok(json!({
        "officialMarketplace": official.marketplace_root.is_some(),
        "officialRegistered": official.config_registered,
        "officialPath": official.marketplace_root,
        "remoteMarketplace": remote_status.marketplace_root.is_some(),
        "remoteRegistered": remote_status.config_registered,
        "remotePath": remote_status.marketplace_root,
        "initializedRemote": initialized_remote || remote.initialized,
        "configuredRemote": remote.configured,
        "configChanged": curated_changed || role_changed || remote.configured || initialized_remote,
    }))
}

/// Reads marketplace availability and registration without creating files or
/// changing Codex configuration. Repairs are deliberately kept in
/// `ensure_marketplaces` so opening Codey settings remains side-effect free.
pub fn marketplaces_status(home: &Path) -> Value {
    let official = codey_runtime_core::plugin_marketplace::openai_curated_marketplace_status(home);
    let remote =
        codey_runtime_core::plugin_marketplace::openai_curated_remote_marketplace_status(home);
    let official_marketplace = official.marketplace_root.is_some();
    let remote_marketplace = remote.marketplace_root.is_some();
    let needs_repair = !official_marketplace
        || !official.config_registered
        || !remote_marketplace
        || !remote.config_registered;
    json!({
        "officialMarketplace": official_marketplace,
        "officialRegistered": official.config_registered,
        "officialPath": official.marketplace_root,
        "remoteMarketplace": remote_marketplace,
        "remoteRegistered": remote.config_registered,
        "remotePath": remote.marketplace_root,
        "needsRepair": needs_repair,
    })
}

pub fn list_plugins(home: &Path) -> Result<Value> {
    let installed = installed_plugins(home)?;
    let mut plugins = Vec::new();
    for marketplace_path in marketplace_paths(home) {
        let bytes = match fs::read(&marketplace_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                error_log::record_failure(
                    "patch_status_failed",
                    "read_plugin_marketplace_file",
                    error.to_string(),
                    serde_json::json!({
                        "path": marketplace_path,
                    }),
                );
                continue;
            }
        };
        let mut marketplace = match serde_json::from_slice::<Value>(&bytes) {
            Ok(marketplace) => marketplace,
            Err(error) => {
                error_log::record_failure(
                    "patch_status_failed",
                    "parse_plugin_marketplace_file",
                    error.to_string(),
                    serde_json::json!({
                        "path": marketplace_path,
                    }),
                );
                continue;
            }
        };
        let marketplace_name = marketplace
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("local")
            .to_string();
        let root = marketplace_path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".tmp").join("plugins"));
        let Some(entries) = marketplace
            .get_mut("plugins")
            .and_then(Value::as_array_mut)
            .map(std::mem::take)
        else {
            continue;
        };
        for entry in entries {
            let Value::Object(mut object) = entry else {
                continue;
            };
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| {
                    object
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| id.split('@').next())
                })
                .unwrap_or_default()
                .trim()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let plugin_root = root.join("plugins").join(&name);
            let id = format!("{name}@{marketplace_name}");
            object.insert("name".into(), Value::String(name.clone()));
            object.insert("id".into(), Value::String(id.clone()));
            object.insert(
                "marketplaceName".into(),
                Value::String(marketplace_name.clone()),
            );
            object.insert(
                "marketplacePath".into(),
                Value::String(marketplace_path.to_string_lossy().to_string()),
            );
            object.insert(
                "localPath".into(),
                Value::String(plugin_root.to_string_lossy().to_string()),
            );
            object.insert("installed".into(), Value::Bool(installed.contains(&id)));
            merge_manifest(&mut object, &plugin_root);
            plugins.push(Value::Object(object));
        }
    }
    let count = plugins.len();
    Ok(json!({"plugins": plugins, "count": count}))
}

fn marketplace_paths(home: &Path) -> [PathBuf; 4] {
    [
        home.join(".tmp/plugins/.agents/plugins/marketplace.json"),
        home.join(".tmp/plugins/.agents/plugins/api_marketplace.json"),
        home.join(".tmp/plugins-remote/.agents/plugins/marketplace.json"),
        home.join(".tmp/marketplaces/role-specific-plugins/.agents/plugins/marketplace.json"),
    ]
}

fn merge_manifest(plugin: &mut Map<String, Value>, plugin_root: &Path) {
    let manifest_path = plugin_root.join(".codex-plugin/plugin.json");
    let Ok(bytes) = fs::read(manifest_path) else {
        return;
    };
    let Ok(manifest) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let Some(manifest) = manifest.as_object() else {
        return;
    };
    for key in [
        "displayName",
        "description",
        "keywords",
        "interface",
        "logoPath",
        "composerIconPath",
    ] {
        if let Some(value) = manifest.get(key) {
            plugin
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }
    }
}

fn installed_plugins(home: &Path) -> Result<HashSet<String>> {
    let path = home.join("config.toml");
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(HashSet::new());
    };
    let Ok(document) = text.parse::<DocumentMut>() else {
        return Ok(HashSet::new());
    };
    let Some(table) = document
        .get("plugins")
        .and_then(|item| item.as_table_like())
    else {
        return Ok(HashSet::new());
    };
    Ok(table
        .iter()
        .filter(|(_, item)| {
            item.as_table_like()
                .and_then(|table| table.get("enabled"))
                .and_then(|value| value.as_bool())
                .unwrap_or(true)
        })
        .map(|(key, _)| key.to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_status_is_read_only_when_repair_is_needed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let config_path = home.join("config.toml");
        let original = b"model_provider = \"openai\"\n";
        fs::write(&config_path, original).unwrap();

        let status = marketplaces_status(home);

        assert_eq!(status["needsRepair"], true);
        assert_eq!(status["officialMarketplace"], false);
        assert_eq!(status["remoteMarketplace"], false);
        assert_eq!(fs::read(&config_path).unwrap(), original);
        assert!(!home.join(".tmp").exists());
    }

    #[test]
    fn ensure_marketplaces_materializes_remote_from_official_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let curated = home.join(".tmp").join("plugins");
        let curated_agents = curated.join(".agents").join("plugins");
        let curated_plugin = curated.join("plugins").join("gmail");
        fs::create_dir_all(&curated_agents).unwrap();
        fs::create_dir_all(curated_plugin.join(".codex-plugin")).unwrap();
        fs::write(
            curated_agents.join("marketplace.json"),
            r#"{"name":"openai-curated","plugins":[{"name":"gmail","source":{"source":"local","path":"./plugins/gmail"}}]}"#,
        )
        .unwrap();
        fs::write(
            curated_plugin.join(".codex-plugin").join("plugin.json"),
            r#"{"name":"gmail"}"#,
        )
        .unwrap();

        let before = marketplaces_status(home);
        let repair = ensure_marketplaces(home).unwrap();
        let after = marketplaces_status(home);

        assert_eq!(before["remoteMarketplace"], false);
        assert_eq!(before["needsRepair"], true);
        assert_eq!(repair["initializedRemote"], true);
        assert_eq!(repair["remoteMarketplace"], true);
        assert_eq!(repair["remoteRegistered"], true);
        assert_eq!(after["needsRepair"], false);
        assert_eq!(after["remoteMarketplace"], true);
        assert_eq!(after["remoteRegistered"], true);
        assert!(
            home.join(".tmp/plugins-remote/.agents/plugins/marketplace.json")
                .is_file()
        );
    }
}
