use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use toml_edit::{DocumentMut, Item, Table};

use crate::config_manager::ConfigManager;

const OPENAI_CURATED_MARKETPLACE: &str = "openai-curated";
const OPENAI_API_CURATED_MARKETPLACE: &str = "openai-api-curated";
const LEGACY_REMOTE_MARKETPLACE: &str = "openai-curated-remote";
const CODEY_CURATED_MARKETPLACE: &str = "codey-curated";
const ROLE_SPECIFIC_PLUGINS_MARKETPLACE: &str = "role-specific-plugins";
const CODEY_CURATED_MARKETPLACE_ZIP: &[u8] =
    include_bytes!("../../../assets/plugin-marketplaces/openai-curated-remote.zip");

pub fn ensure_openai_curated_marketplace_config(home: &Path) -> anyhow::Result<bool> {
    let mut changed = cleanup_managed_reserved_marketplace_configs(home)?;
    if let Some(remote_marketplace_root) = local_openai_curated_remote_marketplace_root(home)? {
        rewrite_marketplace_name(&remote_marketplace_root)?;
        changed |= ensure_marketplace_configs(
            home,
            &[CODEY_CURATED_MARKETPLACE],
            &remote_marketplace_root,
        )?;
    }
    Ok(changed)
}

pub fn ensure_openai_curated_remote_marketplace_config(home: &Path) -> anyhow::Result<bool> {
    let Some(marketplace_root) = local_openai_curated_remote_marketplace_root(home)? else {
        return Ok(false);
    };
    rewrite_marketplace_name(&marketplace_root)?;
    ensure_marketplace_configs(home, &[CODEY_CURATED_MARKETPLACE], &marketplace_root)
}

pub fn ensure_role_specific_plugins_marketplace_config(home: &Path) -> anyhow::Result<bool> {
    let Some(marketplace_root) = local_role_specific_plugins_marketplace_root(home)? else {
        return Ok(false);
    };
    let plugin_ids =
        local_marketplace_plugin_names(&marketplace_root, ROLE_SPECIFIC_PLUGINS_MARKETPLACE)?
            .into_iter()
            .map(|name| format!("{name}@{ROLE_SPECIFIC_PLUGINS_MARKETPLACE}"))
            .collect::<Vec<_>>();
    ensure_marketplace_configs_with_plugins(
        home,
        &[ROLE_SPECIFIC_PLUGINS_MARKETPLACE],
        &marketplace_root,
        &plugin_ids,
    )
}

pub fn ensure_openai_curated_remote_marketplace_available(
    home: &Path,
) -> anyhow::Result<MarketplaceEnsureResult> {
    let mut initialized = false;
    if local_openai_curated_remote_marketplace_root(home)?.is_none() {
        install_openai_curated_remote_marketplace_zip(home, CODEY_CURATED_MARKETPLACE_ZIP)?;
        initialized = true;
    }
    let configured = ensure_openai_curated_remote_marketplace_config(home)?;
    Ok(MarketplaceEnsureResult {
        initialized,
        configured,
    })
}

pub fn preserve_openai_curated_remote_marketplace_config(
    home: &Path,
    config_text: &str,
) -> anyhow::Result<String> {
    let Some(marketplace_root) = local_openai_curated_remote_marketplace_root(home)? else {
        return Ok(config_text.to_string());
    };
    merge_marketplace_configs_into_text(
        config_text,
        &[CODEY_CURATED_MARKETPLACE],
        &marketplace_root,
    )
}

pub fn openai_curated_marketplace_status(home: &Path) -> MarketplaceStatus {
    let marketplace_root = local_openai_curated_marketplace_root(home).ok().flatten();
    let remote_marketplace_root = local_openai_curated_remote_marketplace_root(home)
        .ok()
        .flatten();
    let config_registered = !managed_reserved_marketplace_config_present(home)
        && remote_marketplace_root
            .as_deref()
            .map(|remote_root| {
                marketplace_config_points_to_root(home, CODEY_CURATED_MARKETPLACE, remote_root)
            })
            .unwrap_or(true);
    MarketplaceStatus {
        marketplace_root,
        config_registered,
    }
}

pub fn openai_curated_remote_marketplace_status(home: &Path) -> MarketplaceStatus {
    let marketplace_root = local_openai_curated_remote_marketplace_root(home)
        .ok()
        .flatten();
    let config_registered = marketplace_root
        .as_deref()
        .map(|root| marketplace_config_points_to_root(home, CODEY_CURATED_MARKETPLACE, root))
        .unwrap_or(false);
    MarketplaceStatus {
        marketplace_root,
        config_registered,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketplaceStatus {
    pub marketplace_root: Option<PathBuf>,
    pub config_registered: bool,
}

impl MarketplaceStatus {
    pub fn needs_repair(&self) -> bool {
        self.marketplace_root.is_none() || !self.config_registered
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketplaceEnsureResult {
    pub initialized: bool,
    pub configured: bool,
}

fn local_openai_curated_marketplace_root(home: &Path) -> anyhow::Result<Option<PathBuf>> {
    let root = home.join(".tmp").join("plugins");
    let marketplace_path = root
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    if !marketplace_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&marketplace_path)
        .with_context(|| format!("failed to read {}", marketplace_path.display()))?;
    let marketplace: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", marketplace_path.display()))?;
    if marketplace.get("name").and_then(serde_json::Value::as_str)
        != Some(OPENAI_CURATED_MARKETPLACE)
    {
        return Ok(None);
    }
    let has_plugins = marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .map(|plugins| !plugins.is_empty())
        .unwrap_or(false);
    if !has_plugins || !root.join("plugins").is_dir() {
        return Ok(None);
    }
    Ok(Some(root))
}

fn local_role_specific_plugins_marketplace_root(home: &Path) -> anyhow::Result<Option<PathBuf>> {
    let root = home
        .join(".tmp")
        .join("marketplaces")
        .join(ROLE_SPECIFIC_PLUGINS_MARKETPLACE);
    local_marketplace_root_from_root(&root, ROLE_SPECIFIC_PLUGINS_MARKETPLACE)
}

fn local_marketplace_root_from_root(
    root: &Path,
    marketplace_name: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let marketplace_path = root
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    if !marketplace_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&marketplace_path)
        .with_context(|| format!("failed to read {}", marketplace_path.display()))?;
    let marketplace: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", marketplace_path.display()))?;
    if marketplace.get("name").and_then(serde_json::Value::as_str) != Some(marketplace_name) {
        return Ok(None);
    }
    let has_plugins = marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .map(|plugins| !plugins.is_empty())
        .unwrap_or(false);
    if !has_plugins || !root.join("plugins").is_dir() {
        return Ok(None);
    }
    Ok(Some(root.to_path_buf()))
}

fn local_marketplace_plugin_names(
    root: &Path,
    marketplace_name: &str,
) -> anyhow::Result<Vec<String>> {
    let marketplace_path = root
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    let text = std::fs::read_to_string(&marketplace_path)
        .with_context(|| format!("failed to read {}", marketplace_path.display()))?;
    let marketplace: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", marketplace_path.display()))?;
    if marketplace.get("name").and_then(serde_json::Value::as_str) != Some(marketplace_name) {
        return Ok(Vec::new());
    }
    Ok(marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|plugin| {
            plugin
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .collect())
}

fn local_openai_curated_remote_marketplace_root(home: &Path) -> anyhow::Result<Option<PathBuf>> {
    let root = home.join(".tmp").join("plugins-remote");
    let marketplace_path = root
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    if !marketplace_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&marketplace_path)
        .with_context(|| format!("failed to read {}", marketplace_path.display()))?;
    let marketplace: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", marketplace_path.display()))?;
    if !is_codey_curated_marketplace_name(
        marketplace.get("name").and_then(serde_json::Value::as_str),
    ) {
        return Ok(None);
    }
    let has_plugins = marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .map(|plugins| !plugins.is_empty())
        .unwrap_or(false);
    if !has_plugins || !root.join("plugins").is_dir() {
        return Ok(None);
    }
    Ok(Some(root))
}

fn is_codey_curated_marketplace_name(name: Option<&str>) -> bool {
    matches!(
        name,
        Some(CODEY_CURATED_MARKETPLACE) | Some(LEGACY_REMOTE_MARKETPLACE)
    )
}

fn rewrite_marketplace_name(root: &Path) -> anyhow::Result<()> {
    let path = root
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut marketplace: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if marketplace.get("name").and_then(serde_json::Value::as_str)
        == Some(CODEY_CURATED_MARKETPLACE)
    {
        return Ok(());
    }
    marketplace["name"] = serde_json::Value::String(CODEY_CURATED_MARKETPLACE.to_string());
    let encoded = serde_json::to_vec_pretty(&marketplace)
        .with_context(|| format!("failed to encode {}", path.display()))?;
    crate::settings::atomic_write(&path, &encoded)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn install_openai_curated_remote_marketplace_zip(home: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let destination = home.join(".tmp").join("plugins-remote");
    let staging_parent = home.join(".tmp");
    std::fs::create_dir_all(&staging_parent)
        .with_context(|| format!("failed to create {}", staging_parent.display()))?;
    let staging = staging_parent.join(format!(
        "plugins-remote-embedded-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("failed to remove stale {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("failed to create {}", staging.display()))?;

    let result = extract_zip_exact(bytes, &staging)
        .and_then(|_| rewrite_marketplace_name(&staging))
        .and_then(|_| validate_openai_curated_remote_marketplace_root(&staging))
        .and_then(|_| {
            replace_directory_with_backup_name(
                &staging,
                &destination,
                "plugins-remote.previous-codey",
            )
        });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn extract_zip_exact(bytes: &[u8], destination: &Path) -> anyhow::Result<()> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("failed to read embedded plugin zip")?;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .with_context(|| format!("failed to read zip entry {index}"))?;
        let relative_path = safe_zip_path(file.name())?;
        let output_path = destination.join(relative_path);
        if file.is_dir() {
            std::fs::create_dir_all(&output_path)
                .with_context(|| format!("failed to create {}", output_path.display()))?;
            continue;
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .with_context(|| format!("failed to read zip entry {}", file.name()))?;
        std::fs::write(&output_path, contents)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
    }
    Ok(())
}

fn safe_zip_path(name: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(name);
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => relative.push(value),
            Component::CurDir => {}
            _ => anyhow::bail!("zip entry escapes destination: {name}"),
        }
    }
    if relative.as_os_str().is_empty() {
        anyhow::bail!("zip entry has empty path");
    }
    Ok(relative)
}

fn validate_openai_curated_remote_marketplace_root(root: &Path) -> anyhow::Result<()> {
    let marketplace = local_openai_curated_remote_marketplace_root_from_root(root)?
        .ok_or_else(|| anyhow::anyhow!("embedded official remote plugin marketplace is invalid"))?;
    if marketplace != root {
        anyhow::bail!("embedded official remote plugin marketplace root mismatch");
    }
    Ok(())
}

fn local_openai_curated_remote_marketplace_root_from_root(
    root: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let marketplace_path = root
        .join(".agents")
        .join("plugins")
        .join("marketplace.json");
    if !marketplace_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&marketplace_path)
        .with_context(|| format!("failed to read {}", marketplace_path.display()))?;
    let marketplace: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", marketplace_path.display()))?;
    if !is_codey_curated_marketplace_name(
        marketplace.get("name").and_then(serde_json::Value::as_str),
    ) {
        return Ok(None);
    }
    let has_plugins = marketplace
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .map(|plugins| !plugins.is_empty())
        .unwrap_or(false);
    if !has_plugins || !root.join("plugins").is_dir() {
        return Ok(None);
    }
    Ok(Some(root.to_path_buf()))
}

fn replace_directory_with_backup_name(
    source: &Path,
    destination: &Path,
    backup_name: &str,
) -> anyhow::Result<()> {
    let backup = destination.with_file_name(backup_name);
    if backup.exists() {
        std::fs::remove_dir_all(&backup)
            .with_context(|| format!("failed to remove {}", backup.display()))?;
    }
    if destination.exists() {
        std::fs::rename(destination, &backup).with_context(|| {
            format!(
                "failed to move {} to {}",
                destination.display(),
                backup.display()
            )
        })?;
    }
    match std::fs::rename(source, destination) {
        Ok(()) => {
            if backup.exists() {
                let _ = std::fs::remove_dir_all(&backup);
            }
            Ok(())
        }
        Err(error) => {
            if backup.exists() {
                let _ = std::fs::rename(&backup, destination);
            }
            Err(error).with_context(|| {
                format!(
                    "failed to move {} to {}",
                    source.display(),
                    destination.display()
                )
            })
        }
    }
}

fn ensure_marketplace_configs(
    home: &Path,
    marketplace_names: &[&str],
    marketplace_root: &Path,
) -> anyhow::Result<bool> {
    ensure_marketplace_configs_with_plugins(home, marketplace_names, marketplace_root, &[])
}

fn ensure_marketplace_configs_with_plugins(
    home: &Path,
    marketplace_names: &[&str],
    marketplace_root: &Path,
    plugin_ids: &[String],
) -> anyhow::Result<bool> {
    let manager = ConfigManager::for_home(home);
    let snapshot = manager.load()?;
    let existing = std::str::from_utf8(snapshot.raw())
        .with_context(|| format!("failed to read UTF-8 {}", manager.path().display()))?;
    let without_bom = existing.trim_start_matches('\u{feff}');
    let updated = merge_marketplace_configs_and_plugins_into_text(
        without_bom,
        marketplace_names,
        marketplace_root,
        plugin_ids,
    )?;
    if updated.as_bytes() == without_bom.as_bytes() {
        return Ok(false);
    }
    manager.replace_text(
        Some(snapshot.revision()),
        &updated,
        "register plugin marketplace without changing provider configuration",
        "plugin_marketplace.ensure_marketplace_configs_with_plugins",
    )?;
    Ok(true)
}

pub fn cleanup_managed_reserved_marketplace_configs(home: &Path) -> anyhow::Result<bool> {
    let manager = ConfigManager::for_home(home);
    let snapshot = manager.load()?;
    let existing = std::str::from_utf8(snapshot.raw())
        .with_context(|| format!("failed to read UTF-8 {}", manager.path().display()))?;
    let mut doc = parse_toml_document(existing)?;
    let official_root = home.join(".tmp").join("plugins");
    let remote_root = home.join(".tmp").join("plugins-remote");
    let managed_entries = [
        (OPENAI_CURATED_MARKETPLACE, official_root.as_path()),
        (OPENAI_API_CURATED_MARKETPLACE, official_root.as_path()),
        (LEGACY_REMOTE_MARKETPLACE, remote_root.as_path()),
    ];
    let mut changed = false;
    let mut remove_marketplaces_table = false;
    if let Some(marketplaces) = doc.get_mut("marketplaces").and_then(Item::as_table_mut) {
        for (marketplace_name, root) in managed_entries {
            let managed = marketplaces
                .get(marketplace_name)
                .and_then(Item::as_table)
                .is_some_and(|table| marketplace_table_points_to_root(table, root));
            if managed {
                marketplaces.remove(marketplace_name);
                changed = true;
            }
        }
        remove_marketplaces_table = marketplaces.is_empty();
    }
    if remove_marketplaces_table {
        doc.as_table_mut().remove("marketplaces");
    }
    if !changed {
        return Ok(false);
    }
    let updated = ensure_trailing_newline(doc.to_string());
    manager.replace_text(
        Some(snapshot.revision()),
        &updated,
        "remove Codey-managed reserved plugin marketplace registrations",
        "plugin_marketplace.cleanup_managed_reserved_marketplace_configs",
    )?;
    Ok(true)
}

fn managed_reserved_marketplace_config_present(home: &Path) -> bool {
    let manager = ConfigManager::for_home(home);
    let Ok(snapshot) = manager.load() else {
        return false;
    };
    let Ok(existing) = std::str::from_utf8(snapshot.raw()) else {
        return false;
    };
    let Ok(doc) = parse_toml_document(existing) else {
        return false;
    };
    let Some(marketplaces) = doc.get("marketplaces").and_then(Item::as_table) else {
        return false;
    };
    let official_root = home.join(".tmp").join("plugins");
    let remote_root = home.join(".tmp").join("plugins-remote");
    [
        (OPENAI_CURATED_MARKETPLACE, official_root.as_path()),
        (OPENAI_API_CURATED_MARKETPLACE, official_root.as_path()),
        (LEGACY_REMOTE_MARKETPLACE, remote_root.as_path()),
    ]
    .into_iter()
    .any(|(marketplace_name, root)| {
        marketplaces
            .get(marketplace_name)
            .and_then(Item::as_table)
            .is_some_and(|table| marketplace_table_points_to_root(table, root))
    })
}

fn marketplace_table_points_to_root(table: &Table, root: &Path) -> bool {
    let source_type = table
        .get("source_type")
        .and_then(Item::as_str)
        .unwrap_or_default();
    let source = table
        .get("source")
        .and_then(Item::as_str)
        .unwrap_or_default();
    source_type == "local" && managed_marketplace_path_matches(source, root)
}

fn merge_marketplace_configs_into_text(
    config_text: &str,
    marketplace_names: &[&str],
    marketplace_root: &Path,
) -> anyhow::Result<String> {
    merge_marketplace_configs_and_plugins_into_text(
        config_text,
        marketplace_names,
        marketplace_root,
        &[],
    )
}

fn merge_marketplace_configs_and_plugins_into_text(
    config_text: &str,
    marketplace_names: &[&str],
    marketplace_root: &Path,
    plugin_ids: &[String],
) -> anyhow::Result<String> {
    let mut doc = parse_toml_document(config_text)?;
    let marketplaces = table_mut_or_insert(&mut doc, "marketplaces")?;
    for marketplace_name in marketplace_names {
        if marketplaces
            .get(marketplace_name)
            .and_then(Item::as_table)
            .is_none()
        {
            marketplaces[marketplace_name] = toml_edit::table();
        }
        marketplaces[marketplace_name]["source_type"] = toml_edit::value("local");
        marketplaces[marketplace_name]["source"] =
            toml_edit::value(marketplace_config_path(marketplace_root));
    }
    if !plugin_ids.is_empty() {
        let plugins = table_mut_or_insert(&mut doc, "plugins")?;
        for plugin_id in plugin_ids {
            let existing_enabled = plugins
                .get(plugin_id)
                .and_then(Item::as_table)
                .and_then(|table| table.get("enabled"))
                .and_then(Item::as_bool);
            if plugins.get(plugin_id).and_then(Item::as_table).is_none() {
                plugins[plugin_id] = toml_edit::table();
            }
            if existing_enabled.is_none() {
                plugins[plugin_id]["enabled"] = toml_edit::value(true);
            }
        }
    }
    Ok(ensure_trailing_newline(doc.to_string()))
}

fn marketplace_config_points_to_root(home: &Path, marketplace_name: &str, root: &Path) -> bool {
    let manager = ConfigManager::for_home(home);
    let Ok(snapshot) = manager.load() else {
        return false;
    };
    let doc = snapshot.document();
    let Some(table) = doc
        .get("marketplaces")
        .and_then(Item::as_table)
        .and_then(|marketplaces| marketplaces.get(marketplace_name))
        .and_then(Item::as_table)
    else {
        return false;
    };
    let source_type = table
        .get("source_type")
        .and_then(Item::as_str)
        .unwrap_or_default();
    let source = table
        .get("source")
        .and_then(Item::as_str)
        .unwrap_or_default();
    source_type == "local" && source == marketplace_config_path(root)
}

fn managed_marketplace_path_matches(value: &str, path: &Path) -> bool {
    let native = path.to_string_lossy();
    value == native || value.strip_prefix(r"\\?\") == Some(native.as_ref())
}

fn marketplace_config_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if !cfg!(windows) || value.starts_with(r"\\?\") {
        value.into_owned()
    } else {
        format!(r"\\?\{value}")
    }
}

fn parse_toml_document(contents: &str) -> anyhow::Result<DocumentMut> {
    let contents = contents.trim_start_matches('\u{feff}');
    if contents.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        contents
            .parse::<DocumentMut>()
            .map_err(|error| anyhow::anyhow!("config.toml TOML parse failed: {error}"))
    }
}

fn table_mut_or_insert<'a>(doc: &'a mut DocumentMut, key: &str) -> anyhow::Result<&'a mut Table> {
    if !doc.as_table().contains_key(key) {
        doc[key] = toml_edit::table();
    }
    if doc.get(key).and_then(Item::as_table).is_none() {
        doc[key] = toml_edit::table();
    }
    doc.get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("{key} must be a TOML table"))
}

fn ensure_trailing_newline(mut contents: String) -> String {
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected_marketplace_path(path: &Path) -> String {
        marketplace_config_path(path)
    }

    fn write_marketplace(home: &Path) {
        let root = home.join(".tmp").join("plugins");
        std::fs::create_dir_all(root.join(".agents").join("plugins")).unwrap();
        std::fs::create_dir_all(root.join("plugins").join("gmail")).unwrap();
        std::fs::write(
            root.join(".agents")
                .join("plugins")
                .join("marketplace.json"),
            r#"{"name":"openai-curated","plugins":[{"name":"gmail","path":"./plugins/gmail"}]}"#,
        )
        .unwrap();
    }

    fn write_remote_marketplace(home: &Path) {
        let root = home.join(".tmp").join("plugins-remote");
        std::fs::create_dir_all(root.join(".agents").join("plugins")).unwrap();
        std::fs::create_dir_all(root.join("plugins").join("product-design")).unwrap();
        std::fs::write(
            root.join(".agents")
                .join("plugins")
                .join("marketplace.json"),
            r#"{"name":"openai-curated-remote","plugins":[{"name":"product-design","path":"./plugins/product-design"}]}"#,
        )
        .unwrap();
    }

    fn write_role_specific_marketplace(home: &Path) {
        let root = home
            .join(".tmp")
            .join("marketplaces")
            .join("role-specific-plugins");
        std::fs::create_dir_all(root.join(".agents").join("plugins")).unwrap();
        for plugin in [
            "sales",
            "data-analytics",
            "product-design",
            "financial-markets",
            "customer-support",
        ] {
            std::fs::create_dir_all(root.join("plugins").join(plugin)).unwrap();
        }
        std::fs::write(
            root.join(".agents")
                .join("plugins")
                .join("marketplace.json"),
            r#"{"name":"role-specific-plugins","plugins":[{"name":"sales"},{"name":"data-analytics"},{"name":"product-design"},{"name":"financial-markets"},{"name":"customer-support"}]}"#,
        )
        .unwrap();
    }

    #[test]
    fn ensure_openai_curated_marketplace_config_migrates_managed_reserved_entries() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        write_marketplace(home);
        write_remote_marketplace(home);
        std::fs::write(
            home.join("config.toml"),
            format!(
                r#"[marketplaces.openai-curated]
source_type = "local"
source = {}

[marketplaces.openai-api-curated]
source_type = "local"
source = {}

[marketplaces.openai-curated-remote]
source_type = "local"
source = {}
"#,
                toml_edit::value(marketplace_config_path(&home.join(".tmp/plugins"))),
                toml_edit::value(marketplace_config_path(&home.join(".tmp/plugins"))),
                toml_edit::value(marketplace_config_path(&home.join(".tmp/plugins-remote"))),
            ),
        )
        .unwrap();

        let changed = ensure_openai_curated_marketplace_config(home).unwrap();

        assert!(changed);
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        let marketplaces = parsed["marketplaces"].as_table().unwrap();
        assert!(marketplaces.get(OPENAI_CURATED_MARKETPLACE).is_none());
        assert!(marketplaces.get(OPENAI_API_CURATED_MARKETPLACE).is_none());
        assert!(marketplaces.get(LEGACY_REMOTE_MARKETPLACE).is_none());
        assert_eq!(
            parsed["marketplaces"][CODEY_CURATED_MARKETPLACE]["source_type"].as_str(),
            Some("local")
        );
        assert_eq!(
            parsed["marketplaces"][CODEY_CURATED_MARKETPLACE]["source"].as_str(),
            Some(expected_marketplace_path(&home.join(".tmp/plugins-remote")).as_str())
        );
        let manifest = std::fs::read_to_string(
            home.join(".tmp/plugins-remote/.agents/plugins/marketplace.json"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&manifest).unwrap()["name"],
            CODEY_CURATED_MARKETPLACE
        );
    }

    #[test]
    fn cleanup_managed_reserved_entries_preserves_user_owned_entries() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        std::fs::write(
            home.join("config.toml"),
            r#"[marketplaces.openai-curated]
source_type = "local"
source = "/opt/user-marketplace"
"#,
        )
        .unwrap();

        assert!(!cleanup_managed_reserved_marketplace_configs(home).unwrap());
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains(r#"source = "/opt/user-marketplace""#));
    }

    #[cfg(not(windows))]
    #[test]
    fn remote_marketplace_repairs_a_legacy_windows_path_on_unix() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let remote_root = home.join(".tmp/plugins-remote");
        write_remote_marketplace(home);
        std::fs::write(
            home.join("config.toml"),
            format!(
                r#"[marketplaces.codey-curated]
source_type = "local"
source = {}
"#,
                toml_edit::value(format!(r"\\?\{}", remote_root.display())),
            ),
        )
        .unwrap();

        assert!(!openai_curated_remote_marketplace_status(home).config_registered);
        assert!(ensure_openai_curated_remote_marketplace_config(home).unwrap());

        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            parsed["marketplaces"][CODEY_CURATED_MARKETPLACE]["source"].as_str(),
            Some(remote_root.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn ensure_openai_curated_marketplace_config_skips_when_snapshot_missing() {
        let temp = tempfile::tempdir().unwrap();

        let changed = ensure_openai_curated_marketplace_config(temp.path()).unwrap();

        assert!(!changed);
        assert!(!temp.path().join("config.toml").exists());
    }

    #[test]
    fn ensure_role_specific_plugins_marketplace_config_repairs_installed_plugin_entries() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        write_role_specific_marketplace(home);
        std::fs::write(
            home.join("config.toml"),
            "model_provider = \"custom\"\nexperimental_bearer_token = \"sk-redacted\"\n",
        )
        .unwrap();

        let changed = ensure_role_specific_plugins_marketplace_config(home).unwrap();

        assert!(changed);
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            parsed["marketplaces"]["role-specific-plugins"]["source_type"].as_str(),
            Some("local")
        );
        assert_eq!(
            parsed["marketplaces"]["role-specific-plugins"]["source"].as_str(),
            Some(
                expected_marketplace_path(&home.join(".tmp/marketplaces/role-specific-plugins"),)
                    .as_str()
            )
        );
        for plugin in [
            "sales@role-specific-plugins",
            "data-analytics@role-specific-plugins",
            "product-design@role-specific-plugins",
            "financial-markets@role-specific-plugins",
            "customer-support@role-specific-plugins",
        ] {
            assert_eq!(parsed["plugins"][plugin]["enabled"].as_bool(), Some(true));
        }
        assert_eq!(
            parsed["experimental_bearer_token"].as_str(),
            Some("sk-redacted")
        );
    }

    #[test]
    fn ensure_role_specific_plugins_marketplace_config_preserves_disabled_plugin_choice() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        write_role_specific_marketplace(home);
        std::fs::write(
            home.join("config.toml"),
            "[plugins.\"sales@role-specific-plugins\"]\nenabled = false\n",
        )
        .unwrap();

        let changed = ensure_role_specific_plugins_marketplace_config(home).unwrap();

        assert!(changed);
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        assert_eq!(
            parsed["plugins"]["sales@role-specific-plugins"]["enabled"].as_bool(),
            Some(false)
        );
        assert_eq!(
            parsed["plugins"]["customer-support@role-specific-plugins"]["enabled"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn openai_curated_marketplace_status_accepts_absent_reserved_config() {
        let temp = tempfile::tempdir().unwrap();
        write_marketplace(temp.path());

        let status = openai_curated_marketplace_status(temp.path());

        assert!(status.marketplace_root.is_some());
        assert!(status.config_registered);
        assert!(!status.needs_repair());
    }

    #[test]
    fn openai_curated_marketplace_status_requires_api_marketplace_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let root = home.join(".tmp").join("plugins");
        write_marketplace(home);
        write_remote_marketplace(home);
        ensure_marketplace_configs(home, &[OPENAI_CURATED_MARKETPLACE], &root).unwrap();

        let status = openai_curated_marketplace_status(home);

        assert!(status.marketplace_root.is_some());
        assert!(!status.config_registered);
        assert!(status.needs_repair());
    }

    #[test]
    fn openai_curated_marketplace_status_tracks_managed_remote_compatibility() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        write_marketplace(home);
        write_remote_marketplace(home);
        ensure_openai_curated_marketplace_config(home).unwrap();

        let official = openai_curated_marketplace_status(home);
        let remote = openai_curated_remote_marketplace_status(home);

        assert!(official.marketplace_root.is_some());
        assert!(official.config_registered);
        assert!(!official.needs_repair());
        assert!(remote.marketplace_root.is_some());
        assert!(remote.config_registered);
        assert!(!remote.needs_repair());
    }

    #[test]
    fn openai_curated_remote_marketplace_status_detects_cached_marketplace() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        write_remote_marketplace(home);

        let status = openai_curated_remote_marketplace_status(home);

        assert_eq!(
            status.marketplace_root,
            Some(home.join(".tmp").join("plugins-remote"))
        );
        assert!(!status.config_registered);
        assert!(status.needs_repair());
    }

    #[test]
    fn ensure_openai_curated_remote_marketplace_config_registers_remote_only() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        write_remote_marketplace(home);

        let changed = ensure_openai_curated_remote_marketplace_config(home).unwrap();

        assert!(changed);
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        let parsed = config.parse::<DocumentMut>().unwrap();
        assert!(
            parsed
                .get("marketplaces")
                .and_then(Item::as_table)
                .and_then(|marketplaces| marketplaces.get("openai-curated"))
                .is_none()
        );
        assert_eq!(
            parsed["marketplaces"][CODEY_CURATED_MARKETPLACE]["source_type"].as_str(),
            Some("local")
        );
        assert_eq!(
            parsed["marketplaces"][CODEY_CURATED_MARKETPLACE]["source"].as_str(),
            Some(expected_marketplace_path(&home.join(".tmp/plugins-remote")).as_str())
        );
        let manifest = std::fs::read_to_string(
            home.join(".tmp/plugins-remote/.agents/plugins/marketplace.json"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&manifest).unwrap()["name"],
            CODEY_CURATED_MARKETPLACE
        );
    }

    #[test]
    fn ensure_openai_curated_remote_marketplace_available_installs_embedded_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();

        let result = ensure_openai_curated_remote_marketplace_available(home).unwrap();

        assert!(result.initialized);
        assert!(result.configured);
        let root = home.join(".tmp").join("plugins-remote");
        assert!(root.join(".agents/plugins/marketplace.json").is_file());
        let marketplace: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join(".agents/plugins/marketplace.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marketplace["name"], CODEY_CURATED_MARKETPLACE);
        assert!(
            marketplace["plugins"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        assert!(home.join("config.toml").is_file());
    }

    #[test]
    fn codey_curated_marketplace_name_is_not_reserved_by_codex() {
        assert!(!CODEY_CURATED_MARKETPLACE.starts_with("openai-"));
        assert_ne!(CODEY_CURATED_MARKETPLACE, LEGACY_REMOTE_MARKETPLACE);
    }

    #[test]
    fn embedded_zip_paths_cannot_escape_the_staging_directory() {
        assert_eq!(
            safe_zip_path("plugins/product-design/plugin.json").unwrap(),
            PathBuf::from("plugins/product-design/plugin.json")
        );
        assert!(safe_zip_path("../outside").is_err());
        assert!(safe_zip_path("/absolute/path").is_err());
    }
}
