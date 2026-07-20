use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use toml_edit::DocumentMut;

/// Repairs the official/local marketplace registration without touching the
/// Codex installation directory. The core crate owns the platform-specific
/// config format and embedded remote snapshot; Codey only exposes a small,
/// renderer-friendly status/list API around it.
pub fn ensure_marketplaces(home: &Path) -> Result<Value> {
    let remote =
        codex_plus_core::plugin_marketplace::ensure_openai_curated_remote_marketplace_available(
            home,
        )
        .context("初始化官方远程插件市场失败")?;
    let curated_changed =
        codex_plus_core::plugin_marketplace::ensure_openai_curated_marketplace_config(home)
            .context("注册官方插件市场失败")?;
    let role_changed =
        codex_plus_core::plugin_marketplace::ensure_role_specific_plugins_marketplace_config(home)
            .context("注册本地工具插件市场失败")?;
    let official = codex_plus_core::plugin_marketplace::openai_curated_marketplace_status(home);
    let remote_status =
        codex_plus_core::plugin_marketplace::openai_curated_remote_marketplace_status(home);
    Ok(json!({
        "officialMarketplace": official.marketplace_root.is_some(),
        "officialRegistered": official.config_registered,
        "officialPath": official.marketplace_root,
        "remoteMarketplace": remote_status.marketplace_root.is_some(),
        "remoteRegistered": remote_status.config_registered,
        "remotePath": remote_status.marketplace_root,
        "initializedRemote": remote.initialized,
        "configuredRemote": remote.configured,
        "configChanged": curated_changed || role_changed,
    }))
}

pub fn list_plugins(home: &Path) -> Result<Value> {
    let installed = installed_plugins(home)?;
    let mut plugins = Vec::new();
    for marketplace_path in marketplace_paths(home) {
        let Ok(text) = fs::read_to_string(&marketplace_path) else {
            continue;
        };
        let Ok(mut marketplace) = serde_json::from_str::<Value>(&text) else {
            continue;
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
        let Some(entries) = marketplace.get_mut("plugins").and_then(Value::as_array_mut) else {
            continue;
        };
        for entry in entries.iter_mut() {
            let Some(object) = entry.as_object_mut() else {
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
            merge_manifest(object, &plugin_root);
            plugins.push(Value::Object(object.clone()));
        }
    }
    let count = plugins.len();
    Ok(json!({"plugins": plugins, "count": count}))
}

fn marketplace_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".tmp/plugins/.agents/plugins/marketplace.json"),
        home.join(".tmp/plugins/.agents/plugins/api_marketplace.json"),
        home.join(".tmp/plugins-remote/.agents/plugins/marketplace.json"),
        home.join(".tmp/marketplaces/role-specific-plugins/.agents/plugins/marketplace.json"),
    ]
}

fn merge_manifest(plugin: &mut Map<String, Value>, plugin_root: &Path) {
    let manifest_path = plugin_root.join(".codex-plugin/plugin.json");
    let Ok(text) = fs::read_to_string(manifest_path) else {
        return;
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&text) else {
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
