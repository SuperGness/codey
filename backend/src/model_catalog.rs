use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::fs_util::atomic_write_private_with_parent as atomic_write;
use crate::model_id;

const MODEL_CATALOG_RELATIVE_PATH: &str = "model-catalogs/codey-official.json";
pub(crate) const THIRD_PARTY_REASONING_EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];
const THIRD_PARTY_REASONING_EFFORT_ALLOWLIST: [&str; 6] =
    ["low", "medium", "high", "xhigh", "max", "ultra"];
pub(crate) const THIRD_PARTY_DEFAULT_REASONING_EFFORT: &str = "low";
const REASONING_LEVEL_DESCRIPTIONS: [(&str, &str); 6] = [
    ("low", "Fast responses with lighter reasoning"),
    (
        "medium",
        "Balances speed and reasoning depth for everyday tasks",
    ),
    ("high", "Greater reasoning depth for complex problems"),
    ("xhigh", "Extra high reasoning depth for complex problems"),
    ("max", "Maximum reasoning depth for the toughest tasks"),
    ("ultra", "Maximum reasoning with automatic task delegation"),
];
const FAST_SERVICE_TIER_ID: &str = "priority";
const FAST_SPEED_TIER_ID: &str = "fast";
const PERSONALITY_PLACEHOLDER: &str = "{{ personality }}";
const OFFICIAL_MODELS: [(&str, &str); 8] = [
    ("gpt-6-astra", "GPT-6-Astra"),
    ("gpt-5.6-sol", "GPT-5.6-Sol"),
    ("gpt-5.6-terra", "GPT-5.6-Terra"),
    ("gpt-5.6-luna", "GPT-5.6-Luna"),
    ("gpt-5.5", "GPT-5.5"),
    ("gpt-5.4", "GPT-5.4"),
    ("gpt-5.4-mini", "GPT-5.4-Mini"),
    ("gpt-5.3-codex-spark", "GPT-5.3-Codex-Spark"),
];

#[derive(Debug)]
struct RuntimeModelCacheUnavailable;

impl fmt::Display for RuntimeModelCacheUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "本机 Codex 模型缓存缺少运行时必需字段；请先直接启动官方 Codex 完成模型缓存刷新",
        )
    }
}

impl std::error::Error for RuntimeModelCacheUnavailable {}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialModel {
    pub slug: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialModelAvailability {
    pub slug: String,
    pub display_name: String,
    pub supported: bool,
    pub supported_reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ThirdPartyModelAvailability {
    pub slug: String,
    pub supported_reasoning_efforts: Vec<String>,
    pub default_reasoning_effort: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectionState {
    pub official_models: Vec<OfficialModelAvailability>,
    pub official_model_ids: Vec<String>,
    pub third_party_models: Vec<String>,
    pub third_party_model_metadata: Vec<ThirdPartyModelAvailability>,
    pub manual_third_party_models: Vec<String>,
    pub upstream_models: Vec<String>,
    pub default_model: String,
}

impl ModelSelectionState {
    pub fn available_model(&self, requested: &str) -> Option<&str> {
        let requested = requested.trim();
        if requested.is_empty() {
            return None;
        }
        self.official_models
            .iter()
            .find(|model| model.supported && model.slug.eq_ignore_ascii_case(requested))
            .map(|model| model.slug.as_str())
            .or_else(|| {
                self.third_party_models
                    .iter()
                    .find(|model| model.eq_ignore_ascii_case(requested))
                    .map(String::as_str)
            })
    }

    pub fn first_available_model(&self) -> Option<&str> {
        self.official_models
            .iter()
            .find(|model| model.supported)
            .map(|model| model.slug.as_str())
            .or_else(|| self.third_party_models.first().map(String::as_str))
    }
}

pub fn relative_path() -> &'static str {
    MODEL_CATALOG_RELATIVE_PATH
}

#[derive(Debug)]
pub(crate) struct CatalogSnapshot {
    path: PathBuf,
    contents: Option<Vec<u8>>,
}

pub(crate) fn snapshot(home: &Path) -> Result<CatalogSnapshot> {
    let path = home.join(relative_path());
    let contents = match fs::read(&path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取现有 Codey 模型目录失败：{}", path.display()));
        }
    };
    Ok(CatalogSnapshot { path, contents })
}

pub(crate) fn restore_snapshot(snapshot: CatalogSnapshot) -> Result<()> {
    match snapshot.contents {
        Some(contents) => atomic_write(&snapshot.path, &contents),
        None => match fs::remove_file(&snapshot.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!("移除新建的 Codey 模型目录失败：{}", snapshot.path.display())
            }),
        },
    }
}

pub fn default_official_model_slugs() -> Vec<String> {
    OFFICIAL_MODELS
        .iter()
        .map(|(slug, _)| (*slug).to_string())
        .collect()
}

#[cfg(test)]
pub fn refresh_for_provider(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
) -> Result<usize> {
    refresh_for_provider_with_transport_preferences(
        home,
        official_provider,
        upstream_models,
        selected_models,
        None,
        None,
    )
}

#[cfg(test)]
pub(crate) fn refresh_for_provider_with_websocket_models(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    websocket_models: &[String],
) -> Result<usize> {
    refresh_for_provider_with_transport_preferences(
        home,
        official_provider,
        upstream_models,
        selected_models,
        Some(websocket_models),
        None,
    )
}

pub(crate) fn refresh_for_provider_with_capabilities(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    websocket_models: &[String],
    native_web_search_models: &[String],
) -> Result<usize> {
    refresh_for_provider_with_transport_preferences(
        home,
        official_provider,
        upstream_models,
        selected_models,
        Some(websocket_models),
        Some(native_web_search_models),
    )
}

fn refresh_for_provider_with_transport_preferences(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    websocket_models: Option<&[String]>,
    native_web_search_models: Option<&[String]>,
) -> Result<usize> {
    let official_models = read_official_entries(home)?;
    if official_models
        .iter()
        .all(|model| model_instruction_source(model).is_none())
    {
        return Err(RuntimeModelCacheUnavailable.into());
    }
    let official_slugs = official_models
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(model_id::key)
        .collect::<HashSet<_>>();
    let selected_model_keys = selected_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let provider_models_synced = official_provider || upstream_models.is_some();
    let upstream = upstream_models
        .unwrap_or_default()
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let mut catalog_models = official_models
        .iter()
        .filter(|model| {
            let slug = model.get("slug").and_then(Value::as_str);
            if official_provider {
                return slug.is_some_and(|slug| {
                    selected_model_keys.is_empty()
                        || selected_model_keys.contains(&model_id::key(slug))
                });
            }
            !provider_models_synced
                || slug.is_some_and(|slug| upstream.contains(&model_id::key(slug)))
        })
        .cloned()
        .collect::<Vec<_>>();

    for model in &mut catalog_models {
        let declares_fast_support = declares_fast_speed_support(model);
        ensure_catalog_compatibility(model);
        expose_supported_model(model);
        if declares_fast_support {
            add_fast_speed_controls(model);
        }
    }

    if !official_provider {
        let template = official_models
            .iter()
            .find(|model| {
                model.get("visibility").and_then(Value::as_str) == Some("list")
                    && model_instruction_source(model).is_some()
            })
            .or_else(|| official_models.first())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("官方账号模型缓存为空，请先使用官方账号启动一次 Codex")
            })?;
        let mut seen = HashSet::new();
        for (index, model_id) in selected_models.iter().enumerate() {
            let model_id = model_id.trim();
            let model_key = model_id::key(model_id);
            if model_id.is_empty()
                || official_slugs.contains(&model_key)
                || (provider_models_synced && !upstream.contains(&model_key))
                || !seen.insert(model_key)
            {
                continue;
            }
            let source_template =
                official_template_for_route_alias(official_models.as_slice(), model_id);
            let (source_template, preserve_source_runtime_metadata) = source_template
                .map(|source_template| (source_template, true))
                .unwrap_or((&template, false));
            catalog_models.push(synthetic_model(
                source_template,
                model_id,
                index,
                preserve_source_runtime_metadata,
            ));
        }
    }
    if let Some(websocket_models) = websocket_models {
        let websocket_model_keys = websocket_models
            .iter()
            .map(|model| model_id::key(model))
            .collect::<HashSet<_>>();
        for model in &mut catalog_models {
            let prefer_websockets = model
                .get("slug")
                .and_then(Value::as_str)
                .is_some_and(|slug| websocket_model_keys.contains(&model_id::key(slug)));
            model["prefer_websockets"] = json!(prefer_websockets);
        }
    }
    let native_web_search_model_keys = native_web_search_models
        .unwrap_or_default()
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    for model in &mut catalog_models {
        gate_synthetic_native_web_search(model, &native_web_search_model_keys);
    }
    // Validate the selected models so a newly bundled model without a local
    // runtime template cannot block routes that still use older models.
    if !catalog_models.is_empty() {
        ensure_runtime_compatible_models(&catalog_models)?;
    }
    write_catalog(home, &catalog_models)?;
    let written_models = read_runtime_catalog_models(home)?;
    let expected_model_keys = catalog_models
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(model_id::key)
        .collect::<Vec<_>>();
    let written_model_keys = written_models
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(model_id::key)
        .collect::<Vec<_>>();
    if written_model_keys != expected_model_keys {
        bail!("写入后的 Codey 模型目录与本次生成结果不一致");
    }
    Ok(catalog_models.len())
}

#[cfg(test)]
pub fn selection_state(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    requested_default_model: Option<&str>,
) -> Result<ModelSelectionState> {
    selection_state_with_manual_models(
        home,
        official_provider,
        upstream_models,
        selected_models,
        &[],
        requested_default_model,
    )
}

pub fn selection_state_with_manual_models(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    manual_third_party_models: &[String],
    requested_default_model: Option<&str>,
) -> Result<ModelSelectionState> {
    // Model provenance comes from the route, not from a slug prefix. An API-key
    // provider may legitimately expose a model whose id also appears in the
    // official catalog; it must remain a route-scoped model and go through the
    // local router instead of acquiring official-account semantics.
    let official_entries = match read_official_entries(home) {
        Ok(entries) => entries,
        Err(error) if official_provider => return Err(error),
        Err(_) => Arc::new(Vec::new()),
    };
    let official_model_ids = official_entries
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let selected_official_keys = selected_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let filter_official_selection = official_provider && !selected_official_keys.is_empty();
    let provider_models_synced = official_provider || upstream_models.is_some();
    let upstream_models = upstream_models.unwrap_or_default();
    let upstream = upstream_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let official_models: Vec<OfficialModelAvailability> = if official_provider {
        official_entries
            .iter()
            .filter_map(|model| {
                let supported_reasoning_efforts = reasoning_efforts_from_value(model);
                let default_reasoning_effort =
                    default_reasoning_effort_from_value(model, &supported_reasoning_efforts);
                let model = official_model_from_value(model)?;
                let supported = !filter_official_selection
                    || selected_official_keys.contains(&model_id::key(&model.slug));
                Some(OfficialModelAvailability {
                    slug: model.slug,
                    display_name: model.display_name,
                    supported,
                    supported_reasoning_efforts,
                    default_reasoning_effort,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let third_party_models = if official_provider {
        Vec::new()
    } else {
        let mut seen = HashSet::new();
        selected_models
            .iter()
            .filter_map(|model| {
                let model = model.trim();
                let key = model_id::key(model);
                if key.is_empty()
                    || (provider_models_synced && !upstream.contains(&key))
                    || !seen.insert(key)
                {
                    return None;
                }
                Some(model.to_string())
            })
            .collect()
    };
    let manual_model_keys = manual_third_party_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let manual_third_party_models = if official_provider {
        Vec::new()
    } else {
        third_party_models
            .iter()
            .filter(|model| manual_model_keys.contains(&model_id::key(model)))
            .cloned()
            .collect()
    };
    let default_model = effective_default_model(
        &official_models,
        &third_party_models,
        requested_default_model,
    );
    let third_party_model_metadata = if official_provider {
        Vec::new()
    } else {
        third_party_model_metadata_from_entries(&official_entries, &third_party_models)
    };
    Ok(ModelSelectionState {
        official_models,
        official_model_ids,
        third_party_models,
        third_party_model_metadata,
        manual_third_party_models,
        upstream_models: if official_provider {
            Vec::new()
        } else {
            upstream_models.to_vec()
        },
        default_model,
    })
}

fn effective_default_model(
    official_models: &[OfficialModelAvailability],
    third_party_models: &[String],
    requested_default_model: Option<&str>,
) -> String {
    let requested = requested_default_model
        .map(str::trim)
        .filter(|model| !model.is_empty());
    if let Some(requested) = requested {
        if let Some(model) = official_models
            .iter()
            .find(|model| model.supported && model_id::equal(&model.slug, requested))
        {
            return model.slug.clone();
        }
        if let Some(model) = third_party_models
            .iter()
            .find(|model| model_id::equal(model, requested))
        {
            return model.clone();
        }
    }
    official_models
        .iter()
        .find(|model| model.supported)
        .map(|model| model.slug.clone())
        .or_else(|| third_party_models.first().cloned())
        .unwrap_or_default()
}

pub fn is_available(home: &Path) -> bool {
    read_catalog_value(&home.join(relative_path())).is_some_and(|value| {
        let models = catalog_models_from_value(&value);
        runtime_compatible_models(&models)
    })
}

/// Makes a previously generated catalog safe to reuse when the upstream model
/// cache cannot be refreshed. Older catalogs may still advertise native Web
/// Search for a route whose protocol or capability setting has since changed.
/// This fallback is intentionally subtractive: it can remove stale capability
/// metadata, but never invent support that was absent from the cached source.
pub(crate) fn prepare_cached_catalog_for_native_web_search(
    home: &Path,
    native_web_search_models: &[String],
) -> Result<bool> {
    if !is_available(home) {
        return Ok(false);
    }

    let mut models = read_runtime_catalog_models(home)?;
    let allowed_model_keys = native_web_search_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let mut changed = false;
    for model in &mut models {
        let previous = model.clone();
        gate_cached_native_web_search(model, &allowed_model_keys);
        changed |= *model != previous;
    }
    if changed {
        write_catalog(home, &models)?;
    }

    let written_models = read_runtime_catalog_models(home)?;
    let mut safely_gated_models = written_models.clone();
    for model in &mut safely_gated_models {
        gate_cached_native_web_search(model, &allowed_model_keys);
    }
    if safely_gated_models != written_models {
        bail!("复用的 Codey 模型目录仍包含与当前线路不匹配的原生网页搜索能力");
    }
    Ok(true)
}

/// Repairs catalogs written by older Codey versions that copied model-cache
/// entries without Codex's now-required `description` fields on models and
/// their reasoning levels.
pub(crate) fn repair_missing_descriptions(home: &Path) -> Result<bool> {
    let path = home.join(relative_path());
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取待修复的 Codey 模型目录失败：{}", path.display()));
        }
    };
    let mut catalog: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析待修复的 Codey 模型目录失败：{}", path.display()))?;
    let models = catalog
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("待修复的 Codey 模型目录缺少 models 数组"))?;
    if !models.iter().any(model_needs_description_repair) {
        return Ok(false);
    }
    let mut repaired = false;
    for model in models.iter_mut() {
        if !model_has_runtime_description(model) {
            let description = model
                .get("display_name")
                .and_then(Value::as_str)
                .or_else(|| model.get("slug").and_then(Value::as_str))
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(ToString::to_string);
            if let Some(description) = description {
                model["description"] = json!(description);
                repaired = true;
            }
        }
        if let Some(levels) = model
            .get_mut("supported_reasoning_levels")
            .and_then(Value::as_array_mut)
        {
            for level in levels {
                if level_has_runtime_description(level) {
                    continue;
                }
                let effort = level.get("effort").and_then(Value::as_str);
                if let Some(effort) = effort {
                    level["description"] = json!(reasoning_level_description(effort));
                    repaired = true;
                }
            }
        }
    }
    if models.iter().any(model_needs_description_repair) {
        bail!("旧版 Codey 模型目录存在无法自动补全 description 的条目");
    }
    debug_assert!(repaired);

    let mut contents =
        serde_json::to_vec_pretty(&catalog).context("序列化已修复的 Codey 模型目录失败")?;
    contents.push(b'\n');
    atomic_write(&path, &contents)?;
    Ok(true)
}

pub fn is_runtime_model_cache_unavailable(error: &anyhow::Error) -> bool {
    error.is::<RuntimeModelCacheUnavailable>()
}

/// Signature of the catalog source files, used to reuse a parse across the
/// back-to-back `refresh_for_provider` + `selection_state` calls on every
/// launch and across repeated config-page lookups. The paths are part of the
/// key so entries can never leak between Codex homes.
type CatalogSignature = Vec<(PathBuf, u64, Option<std::time::SystemTime>)>;
type OfficialEntriesCache =
    std::sync::Mutex<Option<(CatalogSignature, std::sync::Arc<Vec<Value>>)>>;

static OFFICIAL_ENTRIES_CACHE: std::sync::OnceLock<OfficialEntriesCache> =
    std::sync::OnceLock::new();

fn catalog_signature(paths: &[PathBuf]) -> CatalogSignature {
    paths
        .iter()
        .map(|path| match fs::metadata(path) {
            Ok(metadata) => (path.clone(), metadata.len(), metadata.modified().ok()),
            Err(_) => (path.clone(), 0, None),
        })
        .collect()
}

fn read_official_entries(home: &Path) -> Result<std::sync::Arc<Vec<Value>>> {
    let paths = vec![home.join("models_cache.json"), home.join(relative_path())];
    let signature = catalog_signature(&paths);
    let cache = OFFICIAL_ENTRIES_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((cached_signature, entries)) = guard.as_ref()
        && *cached_signature == signature
    {
        // 缓存命中只递增引用计数；下游要么只读、要么本来就会拷贝出自己的
        // 工作副本，无需整目录深拷贝。
        return Ok(std::sync::Arc::clone(entries));
    }
    let entries = std::sync::Arc::new(read_official_entries_uncached(&paths)?);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((signature, std::sync::Arc::clone(&entries)));
    }
    Ok(entries)
}

fn read_official_entries_uncached(paths: &[PathBuf]) -> Result<Vec<Value>> {
    let mut catalogs = Vec::new();
    let mut bundled_fast_model_slugs = HashSet::new();
    let mut last_error = None;
    for path in paths {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let value = match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => value,
            Err(error) => {
                last_error = Some(error.to_string());
                continue;
            }
        };
        let models = official_models_from_value(&value);
        if !models.is_empty() {
            catalogs.push(models);
        }
    }
    if let Some(value) = codey_runtime_core::model_suffix::bundled_model_catalog() {
        let models = official_models_from_value(&value);
        if !models.is_empty() {
            bundled_fast_model_slugs.extend(models.iter().filter_map(|model| {
                declares_fast_speed_support(model)
                    .then(|| model.get("slug").and_then(Value::as_str))
                    .flatten()
                    .map(ToString::to_string)
            }));
            catalogs.push(models);
        }
    }
    if catalogs.is_empty() {
        bail!(
            "{}",
            last_error.unwrap_or_else(|| "找不到可用的 Codex 模型模板".to_string())
        );
    }

    OFFICIAL_MODELS
        .iter()
        .enumerate()
        .map(|(priority, (slug, display_name))| {
            let mut matching_models = catalogs
                .iter()
                .flat_map(|models| models.iter())
                .filter(|model| model.get("slug").and_then(Value::as_str) == Some(*slug));
            let mut model = matching_models
                .next()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Codex 模型模板缺少固定官方模型 {slug}"))?;
            let fallbacks = matching_models.collect::<Vec<_>>();
            complete_reasoning_metadata(&mut model, &fallbacks);
            normalize_official_model(&mut model, slug, display_name, priority);
            if bundled_fast_model_slugs.contains(*slug) {
                add_fast_speed_controls(&mut model);
            } else {
                remove_fast_speed_controls(&mut model);
            }
            Ok(model)
        })
        .collect()
}

fn complete_reasoning_metadata(model: &mut Value, fallbacks: &[&Value]) {
    let current_efforts = reasoning_efforts_from_value(model);
    // Older Codex caches can omit this capability list or reduce it to the
    // default `low` entry. Preserve richer local lists, but repair these two
    // incomplete shapes from the best later catalog.
    let current_is_incomplete =
        current_efforts.is_empty() || (current_efforts.len() == 1 && current_efforts[0] == "low");
    if current_is_incomplete {
        let mut best_effort_count = current_efforts.len();
        let mut best_levels = None;
        for fallback in fallbacks {
            let fallback_efforts = reasoning_efforts_from_value(fallback);
            if fallback_efforts.len() > best_effort_count
                && let Some(levels) = fallback
                    .get("supported_reasoning_levels")
                    .and_then(Value::as_array)
            {
                best_effort_count = fallback_efforts.len();
                best_levels = Some(levels.clone());
            }
        }
        if let Some(levels) = best_levels {
            model["supported_reasoning_levels"] = Value::Array(levels);
        }
    }

    let supported = reasoning_efforts_from_value(model);
    let configured_default_is_valid = model
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|effort| supported.iter().any(|candidate| candidate == effort));
    if configured_default_is_valid {
        return;
    }

    let fallback_default = fallbacks.iter().find_map(|fallback| {
        fallback
            .get("default_reasoning_level")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|effort| supported.iter().any(|candidate| candidate == effort))
            .map(ToString::to_string)
    });
    if let Some(object) = model.as_object_mut() {
        object.remove("default_reasoning_level");
        if let Some(default) = fallback_default {
            object.insert("default_reasoning_level".to_string(), json!(default));
        }
    }
}

fn normalize_official_model(model: &mut Value, slug: &str, display_name: &str, priority: usize) {
    model["slug"] = json!(slug);
    model["display_name"] = json!(display_name);
    if model
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_none_or(|description| description.is_empty())
    {
        model["description"] = json!(display_name);
    }
    model["visibility"] = json!("list");
    model["priority"] = json!(priority);
    model["supported_in_api"] = json!(true);
    if model
        .get("multi_agent_version")
        .and_then(Value::as_str)
        .is_none()
    {
        match slug {
            "gpt-6-astra" | "gpt-5.6-sol" | "gpt-5.6-terra" => {
                model["multi_agent_version"] = json!("v2");
            }
            "gpt-5.6-luna" => {
                model["multi_agent_version"] = json!("v1");
            }
            _ => {}
        }
    }
    if let Some(object) = model.as_object_mut() {
        object.remove("availability_nux");
        object.remove("upgrade");
    }
}

fn official_model_from_value(model: &Value) -> Option<OfficialModel> {
    let slug = model.get("slug")?.as_str()?.trim();
    if slug.is_empty() {
        return None;
    }
    let display_name = model
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(slug);
    Some(OfficialModel {
        slug: slug.to_string(),
        display_name: display_name.to_string(),
    })
}

fn reasoning_efforts_from_value(model: &Value) -> Vec<String> {
    model
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|level| {
            level
                .get("effort")
                .and_then(Value::as_str)
                .or_else(|| level.as_str())
        })
        .map(str::trim)
        .filter(|effort| !effort.is_empty())
        .fold(Vec::<String>::new(), |mut efforts, effort| {
            if !efforts.iter().any(|existing| existing == effort) {
                efforts.push(effort.to_string());
            }
            efforts
        })
}

fn third_party_reasoning_efforts_from_value(model: &Value) -> Vec<String> {
    let mut efforts = fallback_third_party_reasoning_efforts();
    let allow_ultra = third_party_template_supports_ultra(model);
    for effort in reasoning_efforts_from_value(model) {
        let allowed = effort == "max" || (effort == "ultra" && allow_ultra);
        if allowed && !efforts.iter().any(|existing| existing == &effort) {
            efforts.push(effort);
        }
    }
    efforts
}

fn third_party_template_supports_ultra(model: &Value) -> bool {
    let is_supported_gpt = model
        .get("slug")
        .and_then(Value::as_str)
        .is_some_and(|slug| {
            [
                "gpt-6-astra",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
            ]
            .iter()
            .any(|candidate| model_id::equal(slug, candidate))
        });
    is_supported_gpt
        && reasoning_efforts_from_value(model)
            .iter()
            .any(|effort| effort == "ultra")
}

fn third_party_template_supports_coordination(model: &Value) -> bool {
    third_party_template_supports_ultra(model)
        && model
            .get("multi_agent_version")
            .and_then(Value::as_str)
            .is_some_and(|version| matches!(version, "v1" | "v2"))
}

fn default_reasoning_effort_from_value(model: &Value, supported: &[String]) -> String {
    let configured = model
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|effort| supported.iter().any(|candidate| candidate == effort));
    configured
        .map(ToString::to_string)
        .or_else(|| {
            supported
                .iter()
                .find(|effort| effort.as_str() == "low")
                .cloned()
        })
        .or_else(|| supported.first().cloned())
        .unwrap_or_else(|| "low".to_string())
}

fn fallback_third_party_reasoning_efforts() -> Vec<String> {
    THIRD_PARTY_REASONING_EFFORTS
        .iter()
        .map(|effort| (*effort).to_string())
        .collect()
}

fn route_scoped_upstream_model_id(model_id: &str) -> &str {
    let model_id = model_id.trim();
    model_id
        .split_once('/')
        .map(|(_, upstream_model_id)| upstream_model_id.trim())
        .filter(|upstream_model_id| !upstream_model_id.is_empty())
        .unwrap_or(model_id)
}

fn official_entry_for_route_model<'a>(
    official_models: &'a [Value],
    route_model_id: &str,
) -> Option<&'a Value> {
    let upstream_model_id = route_scoped_upstream_model_id(route_model_id);
    official_models.iter().find(|model| {
        model
            .get("slug")
            .and_then(Value::as_str)
            .is_some_and(|slug| model_id::equal(slug, upstream_model_id))
    })
}

fn third_party_model_metadata_from_entries(
    official_entries: &[Value],
    third_party_models: &[String],
) -> Vec<ThirdPartyModelAvailability> {
    let availability = |slug: String, entry: Option<&Value>| {
        let supported_reasoning_efforts = entry
            .map(third_party_reasoning_efforts_from_value)
            .unwrap_or_else(fallback_third_party_reasoning_efforts);
        ThirdPartyModelAvailability {
            slug,
            supported_reasoning_efforts,
            default_reasoning_effort: THIRD_PARTY_DEFAULT_REASONING_EFFORT.to_string(),
        }
    };
    let mut metadata = official_entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("slug")
                .and_then(Value::as_str)
                .map(|slug| availability(slug.to_string(), Some(entry)))
        })
        .collect::<Vec<_>>();
    let mut seen = metadata
        .iter()
        .map(|model| model_id::key(&model.slug))
        .collect::<HashSet<_>>();
    for model in third_party_models {
        if !seen.insert(model_id::key(model)) {
            continue;
        }
        metadata.push(availability(
            model.clone(),
            official_entry_for_route_model(official_entries, model),
        ));
    }
    metadata
}

fn official_models_from_value(value: &Value) -> Vec<Value> {
    catalog_models_from_value(value)
        .into_iter()
        .filter(|model| model.get("codey_source").and_then(Value::as_str) != Some("third_party"))
        .collect()
}

fn catalog_models_from_value(value: &Value) -> Vec<Value> {
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    models
        .iter()
        .filter_map(|model| {
            let slug = model.get("slug")?.as_str()?.trim();
            if slug.is_empty() || !model.is_object() || !seen.insert(model_id::key(slug)) {
                return None;
            }
            let mut model = model.clone();
            model["slug"] = json!(slug);
            Some(model)
        })
        .collect()
}

fn ensure_runtime_compatible_models(models: &[Value]) -> Result<()> {
    if source_models_are_runtime_compatible(models) {
        return Ok(());
    }
    Err(RuntimeModelCacheUnavailable.into())
}

fn source_models_are_runtime_compatible(models: &[Value]) -> bool {
    !models.is_empty()
        && models.iter().all(|model| {
            model_instruction_source(model).is_some() && model_has_runtime_description(model)
        })
}

fn runtime_compatible_models(models: &[Value]) -> bool {
    !models.is_empty()
        && models.iter().all(|model| {
            model
                .get("base_instructions")
                .and_then(Value::as_str)
                .is_some()
                && model_has_runtime_description(model)
        })
}

fn model_instruction_source(model: &Value) -> Option<&str> {
    model
        .get("base_instructions")
        .and_then(Value::as_str)
        .or_else(|| {
            model
                .get("model_messages")
                .and_then(|messages| messages.get("instructions_template"))
                .and_then(Value::as_str)
        })
}

fn legacy_base_instructions(model: &Value) -> Option<String> {
    if let Some(base_instructions) = model.get("base_instructions").and_then(Value::as_str) {
        return Some(base_instructions.to_owned());
    }
    let messages = model.get("model_messages")?;
    let template = messages.get("instructions_template")?.as_str()?;
    let Some(variables) = messages
        .get("instructions_variables")
        .filter(|variables| !variables.is_null())
    else {
        return Some(template.to_owned());
    };
    let personality = variables
        .get("personality_default")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(template.replace(PERSONALITY_PLACEHOLDER, personality))
}

fn model_has_runtime_description(model: &Value) -> bool {
    model
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|description| !description.is_empty())
}

fn reasoning_level_description(effort: &str) -> String {
    REASONING_LEVEL_DESCRIPTIONS
        .iter()
        .find(|(known_effort, _)| *known_effort == effort)
        .map(|(_, description)| (*description).to_string())
        .unwrap_or_else(|| format!("{effort} reasoning"))
}

fn level_has_runtime_description(level: &Value) -> bool {
    level
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|description| !description.is_empty())
}

fn model_needs_description_repair(model: &Value) -> bool {
    !model_has_runtime_description(model)
        || model
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some_and(|levels| {
                levels
                    .iter()
                    .any(|level| !level_has_runtime_description(level))
            })
}

fn clamp_reasoning_efforts(model: &mut Value) {
    if let Some(levels) = model
        .get_mut("supported_reasoning_levels")
        .and_then(Value::as_array_mut)
    {
        levels.retain(|level| {
            level
                .get("effort")
                .and_then(Value::as_str)
                .is_some_and(|effort| THIRD_PARTY_REASONING_EFFORT_ALLOWLIST.contains(&effort))
        });
    }
    let supported = reasoning_efforts_from_value(model);
    let default = model
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !supported.iter().any(|effort| effort == default) {
        model["default_reasoning_level"] = json!(
            supported
                .iter()
                .find(|effort| effort.as_str() == THIRD_PARTY_DEFAULT_REASONING_EFFORT)
                .or_else(|| supported.first())
                .map(String::as_str)
                .unwrap_or(THIRD_PARTY_DEFAULT_REASONING_EFFORT)
        );
    }
}

fn ensure_catalog_compatibility(model: &mut Value) {
    if model
        .get("base_instructions")
        .and_then(Value::as_str)
        .is_none()
    {
        let instructions = legacy_base_instructions(model);
        if let Some(instructions) = instructions {
            model["base_instructions"] = json!(instructions);
        }
    }
    if !model
        .get("supports_reasoning_summaries")
        .is_some_and(Value::is_boolean)
    {
        let supports_reasoning_summaries = model
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some_and(|levels| !levels.is_empty());
        model["supports_reasoning_summaries"] = json!(supports_reasoning_summaries);
    }
}

fn expose_supported_model(model: &mut Value) {
    if model.get("visibility").and_then(Value::as_str) == Some("list") {
        model["supported_in_api"] = json!(true);
    }
}

fn declares_fast_speed_support(model: &Value) -> bool {
    model
        .get("service_tiers")
        .and_then(Value::as_array)
        .is_some_and(|tiers| {
            tiers
                .iter()
                .any(|tier| tier.get("id").and_then(Value::as_str) == Some(FAST_SERVICE_TIER_ID))
        })
        || model
            .get("additional_speed_tiers")
            .and_then(Value::as_array)
            .is_some_and(|tiers| {
                tiers
                    .iter()
                    .any(|tier| tier.as_str() == Some(FAST_SPEED_TIER_ID))
            })
}

fn add_fast_speed_controls(model: &mut Value) {
    let service_tiers = model.get_mut("service_tiers").and_then(Value::as_array_mut);
    if let Some(service_tiers) = service_tiers {
        if !service_tiers
            .iter()
            .any(|tier| tier.get("id").and_then(Value::as_str) == Some(FAST_SERVICE_TIER_ID))
        {
            service_tiers.push(json!({
                "id": FAST_SERVICE_TIER_ID,
                "name": "Fast",
                "description": "1.5x speed, increased usage"
            }));
        }
    } else {
        model["service_tiers"] = json!([{
            "id": FAST_SERVICE_TIER_ID,
            "name": "Fast",
            "description": "1.5x speed, increased usage"
        }]);
    }

    let speed_tiers = model
        .get_mut("additional_speed_tiers")
        .and_then(Value::as_array_mut);
    if let Some(speed_tiers) = speed_tiers {
        if !speed_tiers
            .iter()
            .any(|tier| tier.as_str() == Some(FAST_SPEED_TIER_ID))
        {
            speed_tiers.push(json!(FAST_SPEED_TIER_ID));
        }
    } else {
        model["additional_speed_tiers"] = json!([FAST_SPEED_TIER_ID]);
    }
}

fn remove_fast_speed_controls(model: &mut Value) {
    if let Some(service_tiers) = model.get_mut("service_tiers").and_then(Value::as_array_mut) {
        service_tiers
            .retain(|tier| tier.get("id").and_then(Value::as_str) != Some(FAST_SERVICE_TIER_ID));
    }
    if let Some(speed_tiers) = model
        .get_mut("additional_speed_tiers")
        .and_then(Value::as_array_mut)
    {
        speed_tiers.retain(|tier| tier.as_str() != Some(FAST_SPEED_TIER_ID));
    }
}

fn official_template_for_route_alias<'a>(
    official_models: &'a [Value],
    route_model_id: &str,
) -> Option<&'a Value> {
    if !route_model_id.contains('/') {
        return None;
    }
    official_entry_for_route_model(official_models, route_model_id)
}

fn third_party_reasoning_levels(template: &Value, use_template_metadata: bool) -> Value {
    let efforts = if use_template_metadata {
        third_party_reasoning_efforts_from_value(template)
    } else {
        fallback_third_party_reasoning_efforts()
    };
    Value::Array(
        efforts
            .iter()
            .map(|effort| json!({ "effort": effort, "description": reasoning_level_description(effort) }))
            .collect(),
    )
}

fn synthetic_model(
    template: &Value,
    model_id: &str,
    index: usize,
    preserve_source_runtime_metadata: bool,
) -> Value {
    let preserve_multi_agent_version =
        preserve_source_runtime_metadata && third_party_template_supports_coordination(template);
    let mut model = template.clone();
    if !preserve_source_runtime_metadata {
        codey_runtime_core::model_suffix::sanitize_generic_model_metadata(&mut model);
    }
    model["slug"] = json!(model_id);
    model["display_name"] = json!(model_id);
    model["description"] = json!("Third-party API model");
    model["visibility"] = json!("list");
    model["priority"] = json!(1000 + index);
    model["supported_in_api"] = json!(true);
    model["codey_source"] = json!("third_party");
    model["default_reasoning_level"] = json!(THIRD_PARTY_DEFAULT_REASONING_EFFORT);
    model["supported_reasoning_levels"] =
        third_party_reasoning_levels(template, preserve_source_runtime_metadata);
    if let Some(object) = model.as_object_mut() {
        object.remove("availability_nux");
        object.remove("upgrade");
        // Only route aliases that exactly reuse a supported GPT template with native
        // Ultra support may coordinate delegated work. Generic provider models
        // remain leaf candidates and must not inherit that capability.
        if !preserve_multi_agent_version {
            object.remove("multi_agent_version");
            object.remove("multi_agent_reasoning_effort");
        }
    }
    model["service_tiers"] = json!([]);
    model["additional_speed_tiers"] = json!([]);
    ensure_catalog_compatibility(&mut model);
    clamp_reasoning_efforts(&mut model);
    add_fast_speed_controls(&mut model);
    model
}

fn gate_synthetic_native_web_search(model: &mut Value, allowed_model_keys: &HashSet<String>) {
    if model.get("codey_source").and_then(Value::as_str) != Some("third_party") {
        return;
    }
    gate_cached_native_web_search(model, allowed_model_keys);
}

fn gate_cached_native_web_search(model: &mut Value, allowed_model_keys: &HashSet<String>) {
    let allowed = model
        .get("slug")
        .and_then(Value::as_str)
        .is_some_and(|slug| allowed_model_keys.contains(&model_id::key(slug)));
    let source_declares_search = model.get("supports_search_tool").and_then(Value::as_bool)
        == Some(true)
        && model
            .get("web_search_tool_type")
            .and_then(Value::as_str)
            .is_some_and(|tool_type| !tool_type.trim().is_empty());
    if allowed && source_declares_search {
        return;
    }
    if let Some(object) = model.as_object_mut() {
        object.remove("supports_search_tool");
        object.remove("web_search_tool_type");
    }
}

fn write_catalog(home: &Path, models: &[Value]) -> Result<()> {
    let mut catalog = serde_json::to_vec_pretty(&json!({ "models": models }))
        .context("序列化 Codey 模型目录失败")?;
    catalog.push(b'\n');
    let path = home.join(relative_path());
    if fs::read(&path).is_ok_and(|current| current == catalog) {
        protect_catalog_file(&path)?;
        return Ok(());
    }
    atomic_write(&path, &catalog)
}

fn read_catalog_value(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

fn read_runtime_catalog_models(home: &Path) -> Result<Vec<Value>> {
    let path = home.join(relative_path());
    let bytes = fs::read(&path)
        .with_context(|| format!("读取 Codey 运行时模型目录失败：{}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Codey 运行时模型目录失败：{}", path.display()))?;
    if value.get("models").and_then(Value::as_array).is_none() {
        bail!("Codey 运行时模型目录缺少 models 数组");
    }
    let models = catalog_models_from_value(&value);
    if !models.is_empty() && !runtime_compatible_models(&models) {
        bail!("Codey 运行时模型目录缺少 Codex 必需字段");
    }
    Ok(models)
}

fn protect_catalog_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("保护本地模型目录失败：{}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official_cache() -> Value {
        let mut cache = json!({
            "client_version": "test-client",
            "models": [
                {
                    "slug": "gpt-5.6-sol",
                    "display_name": "GPT-5.6-Sol",
                    "visibility": "list",
                    "priority": 1,
                    "default_reasoning_level": "low",
                    "supported_reasoning_levels": [
                        {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                        {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}
                    ],
                    "use_responses_lite": true,
                    "tool_mode": "code_mode_only",
                    "comp_hash": "3000",
                    "default_service_tier": "priority",
                    "prefer_websockets": true,
                    "include_skills_usage_instructions": false,
                    "include_plugin_usage_instructions": true,
                    "include_apps_usage_instructions": true,
                    "experimental_supported_tools": ["gpt-5.6-only-tool"],
                    "node_repl_auto_review_required": false,
                    "node_repl_disabled": false,
                    "service_tiers": [{"id": "priority"}],
                    "additional_speed_tiers": ["fast"]
                },
                {
                    "slug": "gpt-5.5",
                    "display_name": "GPT-5.5",
                    "visibility": "list",
                    "priority": 7,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [{"effort": "low"}, {"effort": "xhigh"}],
                    "use_responses_lite": false,
                    "comp_hash": "2911",
                    "include_skills_usage_instructions": true,
                    "include_plugin_usage_instructions": true,
                    "include_apps_usage_instructions": true,
                    "experimental_supported_tools": [],
                    "node_repl_auto_review_required": false,
                    "node_repl_disabled": false,
                    "additional_speed_tiers": ["fast"]
                },
                {
                    "slug": "gpt-5.4",
                    "display_name": "GPT-5.4",
                    "visibility": "hide",
                    "priority": 16,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [
                        {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                        {"effort": "xhigh"}, {"effort": "max"}
                    ],
                    "service_tiers": [{"id": "priority"}],
                    "additional_speed_tiers": ["fast"],
                    "upgrade": {"model": "gpt-5.6-sol"}
                },
                {
                    "slug": "gpt-5.3-codex-spark",
                    "display_name": "GPT-5.3-Codex-Spark",
                    "visibility": "list",
                    "priority": 30,
                    "supported_in_api": false,
                    "default_reasoning_level": "high",
                    "supported_reasoning_levels": [
                        {"effort": "low"}, {"effort": "medium"},
                        {"effort": "high"}, {"effort": "xhigh"}
                    ],
                    "service_tiers": [],
                    "additional_speed_tiers": []
                },
                {"slug": "codex-auto-review", "visibility": "hide", "priority": 43}
            ]
        });
        let bundled = codey_runtime_core::model_suffix::bundled_model_catalog().unwrap();
        for (slug, _) in OFFICIAL_MODELS {
            let exists = cache["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model["slug"] == slug);
            if exists {
                continue;
            }
            let model = bundled["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|model| model["slug"] == slug)
                .unwrap()
                .clone();
            cache["models"].as_array_mut().unwrap().push(model);
        }
        for model in cache["models"].as_array_mut().unwrap() {
            let slug = model["slug"].as_str().unwrap_or("test-model").to_string();
            match slug.as_str() {
                "gpt-6-astra" | "gpt-5.6-sol" | "gpt-5.6-terra" => {
                    model["multi_agent_version"] = json!("v2");
                }
                "gpt-5.6-luna" => {
                    model["multi_agent_version"] = json!("v1");
                }
                "gpt-5.4" => {
                    model["multi_agent_version"] = json!("disabled");
                }
                _ => {
                    model.as_object_mut().unwrap().remove("multi_agent_version");
                }
            }
            model["base_instructions"] = json!(format!("test-only instructions for {slug}"));
            model["model_messages"] = json!({
                "instructions_template": "test-only template"
            });
        }
        cache
    }

    fn write_cache(home: &Path) {
        fs::write(
            home.join("models_cache.json"),
            serde_json::to_vec(&official_cache()).unwrap(),
        )
        .unwrap();
    }

    fn write_cache_with_native_web_search(home: &Path) {
        let mut cache = official_cache();
        let sol = cache["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        sol["supports_search_tool"] = json!(true);
        sol["web_search_tool_type"] = json!("text_and_image");
        fs::write(
            home.join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }

    fn write_cache_with_template_only(home: &Path) {
        let mut cache = official_cache();
        for model in cache["models"].as_array_mut().unwrap() {
            let slug = model["slug"].as_str().unwrap_or("test-model").to_owned();
            model.as_object_mut().unwrap().remove("base_instructions");
            model["model_messages"] = json!({
                "instructions_template": "test-only prefix {{ personality }} suffix",
                "instructions_variables": {
                    "personality_default": format!("test-only default personality for {slug}"),
                    "personality_friendly": "test-only friendly personality",
                    "personality_pragmatic": "test-only pragmatic personality"
                }
            });
        }
        fs::write(
            home.join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }

    fn write_cache_without_fast_metadata(home: &Path) {
        let mut cache = official_cache();
        for model in cache["models"].as_array_mut().unwrap() {
            let object = model.as_object_mut().unwrap();
            object.remove("service_tiers");
            object.remove("additional_speed_tiers");
        }
        fs::write(
            home.join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }

    fn write_cache_with_prompt_fields(home: &Path) {
        let mut cache = official_cache();
        let model = &mut cache["models"][0];
        model["base_instructions"] = json!("runtime-cache-only base instructions");
        model["model_messages"] = json!({
            "instructions_template": "runtime-cache-only template",
            "instructions_variables": {
                "developer": "runtime-cache-only variable"
            }
        });
        model["compatibility"] = json!({
            "instructions_template": "runtime-cache-only nested template",
            "instructions_variables": {
                "nested": "runtime-cache-only nested variable"
            }
        });
        fs::write(
            home.join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }

    fn write_cache_with_sol_reasoning_metadata(
        home: &Path,
        supported_reasoning_levels: Option<Value>,
        default_reasoning_level: Option<&str>,
    ) {
        let mut cache = official_cache();
        let sol = cache["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        let object = sol.as_object_mut().unwrap();
        match supported_reasoning_levels {
            Some(levels) => {
                object.insert("supported_reasoning_levels".to_string(), levels);
            }
            None => {
                object.remove("supported_reasoning_levels");
            }
        }
        match default_reasoning_level {
            Some(default) => {
                object.insert("default_reasoning_level".to_string(), json!(default));
            }
            None => {
                object.remove("default_reasoning_level");
            }
        }
        fs::write(
            home.join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }

    fn assert_native_fast(model: &Value) {
        assert!(
            model["service_tiers"]
                .as_array()
                .is_some_and(|tiers| tiers.iter().any(|tier| tier["id"] == FAST_SERVICE_TIER_ID))
        );
        assert!(
            model["additional_speed_tiers"]
                .as_array()
                .is_some_and(|tiers| tiers.iter().any(|tier| tier == FAST_SPEED_TIER_ID))
        );
    }

    fn assert_no_native_fast(model: &Value) {
        assert!(!declares_fast_speed_support(model));
    }

    #[test]
    fn official_catalog_keeps_the_fixed_order_and_native_fast_metadata() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());

        assert_eq!(
            refresh_for_provider(home.path(), true, None, &[]).unwrap(),
            OFFICIAL_MODELS.len()
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            OFFICIAL_MODELS
                .iter()
                .map(|(slug, _)| *slug)
                .collect::<Vec<_>>()
        );
        assert!(models.iter().all(|model| model["visibility"] == "list"));
        let sol = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(sol["multi_agent_version"], "v2");
        let efforts = sol["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|level| level["effort"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max", "ultra"]);
        assert_eq!(sol["service_tiers"][0]["id"], "priority");
        assert_eq!(sol["supports_reasoning_summaries"], true);
        let gpt_55 = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.5")
            .unwrap();
        assert_eq!(gpt_55["service_tiers"][0]["id"], "priority");
        assert!(gpt_55.get("multi_agent_version").is_none());
        let luna = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-luna")
            .unwrap();
        assert_eq!(luna["multi_agent_version"], "v1");
        let gpt_54 = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.4")
            .unwrap();
        assert_eq!(gpt_54["multi_agent_version"], "disabled");
        let spark = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.3-codex-spark")
            .unwrap();
        assert_eq!(spark["supported_in_api"], true);
        assert_eq!(spark["supports_reasoning_summaries"], true);
        assert_no_native_fast(spark);
        let mini = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.4-mini")
            .unwrap();
        assert_no_native_fast(mini);
        assert_eq!(
            models
                .iter()
                .filter(|model| declares_fast_speed_support(model))
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "gpt-6-astra",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-5.5",
                "gpt-5.4",
            ]
        );
    }

    #[test]
    fn generated_catalog_preserves_official_multi_agent_markers() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());

        refresh_for_provider(home.path(), true, None, &[]).unwrap();

        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        let marker = |slug: &str| {
            models
                .iter()
                .find(|model| model["slug"] == slug)
                .and_then(|model| model.get("multi_agent_version"))
                .and_then(Value::as_str)
        };

        assert_eq!(marker("gpt-6-astra"), Some("v2"));
        assert_eq!(marker("gpt-5.6-sol"), Some("v2"));
        assert_eq!(marker("gpt-5.6-terra"), Some("v2"));
        assert_eq!(marker("gpt-5.6-luna"), Some("v1"));
        assert_eq!(marker("gpt-5.4"), Some("disabled"));
        assert_eq!(marker("gpt-5.5"), None);
    }

    #[test]
    fn generated_catalog_keeps_leaf_models_without_v2_coordinator_markers() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec![
            "gpt-5.6-luna".into(),
            "gpt-5.4".into(),
            "provider-custom-model".into(),
        ];

        refresh_for_provider(home.path(), false, Some(&upstream), &upstream).unwrap();

        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        let luna = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-luna")
            .unwrap();
        assert_eq!(luna["multi_agent_version"], "v1");
        let gpt_54 = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.4")
            .unwrap();
        assert_eq!(gpt_54["multi_agent_version"], "disabled");
        let custom = models
            .iter()
            .find(|model| model["slug"] == "provider-custom-model")
            .unwrap();
        assert!(custom.get("multi_agent_version").is_none());
    }

    #[test]
    fn generated_catalog_preserves_required_fields_from_the_local_native_cache() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_prompt_fields(home.path());

        refresh_for_provider(home.path(), true, None, &[]).unwrap();

        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let model = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(
            model["base_instructions"],
            "runtime-cache-only base instructions"
        );
        assert_eq!(
            model["model_messages"]["instructions_template"],
            "runtime-cache-only template"
        );
        assert_eq!(
            model["compatibility"]["instructions_variables"]["nested"],
            "runtime-cache-only nested variable"
        );
        assert!(is_available(home.path()));
    }

    #[test]
    fn generated_catalog_derives_base_instructions_from_the_local_template() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_template_only(home.path());

        refresh_for_provider(home.path(), true, None, &[]).unwrap();

        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert!(models.iter().all(|model| {
            let template = model["model_messages"]["instructions_template"]
                .as_str()
                .unwrap();
            let personality =
                model["model_messages"]["instructions_variables"]["personality_default"]
                    .as_str()
                    .unwrap();
            model["base_instructions"] == template.replace(PERSONALITY_PLACEHOLDER, personality)
        }));
        assert!(is_available(home.path()));
    }

    #[test]
    fn generated_catalog_fills_missing_or_empty_official_descriptions() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = official_cache();
        let models = cache["models"].as_array_mut().unwrap();
        models
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap()["description"] = json!("Local Sol description");
        models
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.5")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("description");
        models
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.4")
            .unwrap()["description"] = json!("   ");
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();

        refresh_for_provider(home.path(), true, None, &[]).unwrap();

        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert!(models.iter().all(|model| {
            model
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|description| !description.is_empty())
        }));
        let sol = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(sol["description"], "Local Sol description");
        for slug in ["gpt-5.5", "gpt-5.4"] {
            let model = models.iter().find(|model| model["slug"] == slug).unwrap();
            assert_eq!(model["description"], model["display_name"]);
        }
    }

    #[test]
    fn incomplete_local_reasoning_metadata_is_completed_from_fallback_catalog() {
        let cases = [
            (None, None, "low"),
            (None, Some("xhigh"), "xhigh"),
            (
                Some(json!([{"effort": "low", "description": "local low"}])),
                Some("low"),
                "low",
            ),
        ];

        for (supported_reasoning_levels, default_reasoning_level, expected_default) in cases {
            let home = tempfile::tempdir().unwrap();
            write_cache_with_sol_reasoning_metadata(
                home.path(),
                supported_reasoning_levels,
                default_reasoning_level,
            );

            let state = selection_state(home.path(), true, None, &[], None).unwrap();
            let sol_state = state
                .official_models
                .iter()
                .find(|model| model.slug == "gpt-5.6-sol")
                .unwrap();
            assert_eq!(
                sol_state.supported_reasoning_efforts,
                ["low", "medium", "high", "xhigh", "max", "ultra"]
            );
            assert_eq!(sol_state.default_reasoning_effort, expected_default);

            refresh_for_provider(home.path(), true, None, &[]).unwrap();
            let catalog: Value = serde_json::from_slice(
                &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
            )
            .unwrap();
            let sol = catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|model| model["slug"] == "gpt-5.6-sol")
                .unwrap();
            assert_eq!(
                reasoning_efforts_from_value(sol),
                ["low", "medium", "high", "xhigh", "max", "ultra"]
            );
            assert_eq!(sol["default_reasoning_level"], expected_default);
            assert_eq!(
                sol["base_instructions"],
                "test-only instructions for gpt-5.6-sol"
            );
            assert_eq!(
                sol["model_messages"]["instructions_template"],
                "test-only template"
            );
        }
    }

    #[test]
    fn explicit_nontrivial_local_reasoning_metadata_remains_authoritative() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_sol_reasoning_metadata(
            home.path(),
            Some(json!([
                {"effort": "low", "description": "local low"},
                {"effort": "xhigh", "description": "local xhigh"}
            ])),
            Some("xhigh"),
        );

        let state = selection_state(home.path(), true, None, &[], None).unwrap();
        let sol = state
            .official_models
            .iter()
            .find(|model| model.slug == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(sol.supported_reasoning_efforts, ["low", "xhigh"]);
        assert_eq!(sol.default_reasoning_effort, "xhigh");
    }

    #[cfg(unix)]
    #[test]
    fn generated_catalog_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());

        refresh_for_provider(home.path(), true, None, &[]).unwrap();

        let mode = fs::metadata(home.path().join(MODEL_CATALOG_RELATIVE_PATH))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn native_cache_without_fast_metadata_inherits_bundled_official_capabilities() {
        let home = tempfile::tempdir().unwrap();
        write_cache_without_fast_metadata(home.path());

        refresh_for_provider(home.path(), true, None, &[]).unwrap();

        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(
            models
                .iter()
                .filter(|model| declares_fast_speed_support(model))
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "gpt-6-astra",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "gpt-5.6-luna",
                "gpt-5.5",
                "gpt-5.4",
            ]
        );
        for slug in ["gpt-5.4-mini", "gpt-5.3-codex-spark"] {
            let model = models.iter().find(|model| model["slug"] == slug).unwrap();
            assert_no_native_fast(model);
        }
    }

    #[test]
    fn third_party_catalog_keeps_supported_official_models_before_configured_models() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_template_only(home.path());
        let upstream = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.4".into(),
            "gpt-5.3-codex-spark".into(),
            "claude-sonnet".into(),
        ];
        let selected = upstream.clone();

        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&upstream), &selected,).unwrap(),
            4
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "gpt-5.6-sol",
                "gpt-5.4",
                "gpt-5.3-codex-spark",
                "claude-sonnet",
            ]
        );
        assert_eq!(
            models[0]["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|level| level["effort"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        let gpt_54 = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.4")
            .unwrap();
        assert_eq!(gpt_54["visibility"], "list");
        assert!(gpt_54.get("upgrade").is_none());
        let spark = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.3-codex-spark")
            .unwrap();
        assert_eq!(spark["supported_in_api"], true);
        assert_no_native_fast(spark);
        let custom = models.last().unwrap();
        assert_eq!(custom["slug"], "claude-sonnet");
        assert_eq!(custom["codey_source"], "third_party");
        assert!(custom.get("multi_agent_version").is_none());
        assert_eq!(
            custom["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|level| level["effort"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "xhigh"]
        );
        assert_eq!(custom["supports_reasoning_summaries"], true);
        let custom_template = custom["model_messages"]["instructions_template"]
            .as_str()
            .unwrap();
        let custom_personality =
            custom["model_messages"]["instructions_variables"]["personality_default"]
                .as_str()
                .unwrap();
        assert_eq!(
            custom["base_instructions"],
            custom_template.replace(PERSONALITY_PLACEHOLDER, custom_personality)
        );
        assert_native_fast(custom);
        assert_native_fast(
            models
                .iter()
                .find(|model| model["slug"] == "gpt-5.6-sol")
                .unwrap(),
        );
        assert_native_fast(
            models
                .iter()
                .find(|model| model["slug"] == "gpt-5.4")
                .unwrap(),
        );
    }

    #[test]
    fn route_aliases_use_matching_official_runtime_metadata_and_sanitize_unknown_models() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec![
            "openai/gpt-6-astra".into(),
            "openai/gpt-5.6-sol".into(),
            "openai/gpt-5.5".into(),
            "provider/custom-model".into(),
        ];

        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&selected), &selected).unwrap(),
            4
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();

        let astra = models
            .iter()
            .find(|model| model["slug"] == "openai/gpt-6-astra")
            .unwrap();
        assert_eq!(astra["use_responses_lite"], true);
        assert_eq!(astra["tool_mode"], "code_mode_only");
        assert_eq!(astra["multi_agent_version"], "v2");
        assert_eq!(astra["multi_agent_reasoning_effort"], "xhigh");
        assert_eq!(astra["node_repl_auto_review_required"], true);
        assert_eq!(
            astra["experimental_supported_tools"],
            json!(["send_user_message_async", "clock"])
        );
        assert_eq!(
            reasoning_efforts_from_value(astra),
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(
            astra["base_instructions"],
            "test-only instructions for gpt-6-astra"
        );

        let gpt_56 = models
            .iter()
            .find(|model| model["slug"] == "openai/gpt-5.6-sol")
            .unwrap();
        assert_eq!(gpt_56["use_responses_lite"], true);
        assert_eq!(gpt_56["tool_mode"], "code_mode_only");
        assert_eq!(gpt_56["comp_hash"], "3000");
        assert_eq!(gpt_56["default_service_tier"], "priority");
        assert_eq!(gpt_56["prefer_websockets"], true);
        assert_eq!(gpt_56["include_skills_usage_instructions"], false);
        assert_eq!(
            gpt_56["experimental_supported_tools"],
            json!(["gpt-5.6-only-tool"])
        );
        assert_eq!(
            reasoning_efforts_from_value(gpt_56),
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(gpt_56["multi_agent_version"], "v2");
        assert_eq!(
            gpt_56["base_instructions"],
            "test-only instructions for gpt-5.6-sol"
        );

        let gpt_55 = models
            .iter()
            .find(|model| model["slug"] == "openai/gpt-5.5")
            .unwrap();
        assert_eq!(gpt_55["use_responses_lite"], false);
        assert!(gpt_55.get("tool_mode").is_none());
        assert_eq!(gpt_55["comp_hash"], "2911");
        assert_eq!(gpt_55["include_skills_usage_instructions"], true);
        assert_eq!(gpt_55["experimental_supported_tools"], json!([]));
        assert_eq!(
            reasoning_efforts_from_value(gpt_55),
            ["low", "medium", "high", "xhigh"]
        );
        assert!(gpt_55.get("multi_agent_version").is_none());
        assert_eq!(
            gpt_55["base_instructions"],
            "test-only instructions for gpt-5.5"
        );

        let custom = models
            .iter()
            .find(|model| model["slug"] == "provider/custom-model")
            .unwrap();
        assert_eq!(custom["use_responses_lite"], false);
        for field in [
            "tool_mode",
            "multi_agent_version",
            "multi_agent_reasoning_effort",
            "comp_hash",
            "default_service_tier",
            "prefer_websockets",
            "reasoning_summary_format",
            "auto_review_model_override",
            "node_repl_auto_review_required",
            "node_repl_disabled",
            "supports_search_tool",
            "web_search_tool_type",
        ] {
            assert!(
                custom.get(field).is_none(),
                "unknown model inherited model-specific field {field}"
            );
        }
        assert_eq!(custom["include_skills_usage_instructions"], true);
        assert_eq!(custom["include_plugin_usage_instructions"], true);
        assert_eq!(custom["include_apps_usage_instructions"], true);
        assert_eq!(custom["experimental_supported_tools"], json!([]));
        assert_eq!(
            reasoning_efforts_from_value(custom),
            ["low", "medium", "high", "xhigh"]
        );
        assert!(custom["auto_compact_token_limit"].is_null());
    }

    #[test]
    fn websocket_preference_is_isolated_per_route_model_alias() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec![
            "route-ws/gpt-5.6-sol".to_string(),
            "route-http/gpt-5.6-sol".to_string(),
        ];
        let websocket_models = vec!["route-ws/gpt-5.6-sol".to_string()];

        refresh_for_provider_with_websocket_models(
            home.path(),
            false,
            Some(&selected),
            &selected,
            &websocket_models,
        )
        .unwrap();
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        let websocket = models
            .iter()
            .find(|model| model["slug"] == "route-ws/gpt-5.6-sol")
            .unwrap();
        let http = models
            .iter()
            .find(|model| model["slug"] == "route-http/gpt-5.6-sol")
            .unwrap();

        assert_eq!(websocket["prefer_websockets"], true);
        assert_eq!(http["prefer_websockets"], false);
    }

    #[test]
    fn native_web_search_is_gated_by_route_and_source_model_metadata() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_native_web_search(home.path());
        let selected = vec![
            "route-search/gpt-5.6-sol".to_string(),
            "route-off/gpt-5.6-sol".to_string(),
            "route-search/claude-opus-5".to_string(),
        ];
        let native_web_search_models = vec![
            "route-search/gpt-5.6-sol".to_string(),
            "route-search/claude-opus-5".to_string(),
        ];

        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            &[],
            &native_web_search_models,
        )
        .unwrap();
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        let search = models
            .iter()
            .find(|model| model["slug"] == "route-search/gpt-5.6-sol")
            .unwrap();
        let disabled = models
            .iter()
            .find(|model| model["slug"] == "route-off/gpt-5.6-sol")
            .unwrap();
        let unknown = models
            .iter()
            .find(|model| model["slug"] == "route-search/claude-opus-5")
            .unwrap();

        assert_eq!(search["supports_search_tool"], true);
        assert!(search["web_search_tool_type"].is_string());
        for model in [disabled, unknown] {
            assert!(model.get("supports_search_tool").is_none());
            assert!(model.get("web_search_tool_type").is_none());
        }
    }

    #[test]
    fn cached_catalog_fallback_removes_stale_native_web_search_metadata() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_native_web_search(home.path());
        let selected = vec!["route-search/gpt-5.6-sol".to_string()];

        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            &[],
            &selected,
        )
        .unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        let mut stale: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stale["models"][0]["supports_search_tool"], true);
        assert!(stale["models"][0]["web_search_tool_type"].is_string());
        stale["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("codey_source");
        fs::write(&path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

        assert!(
            prepare_cached_catalog_for_native_web_search(home.path(), &[]).unwrap(),
            "a valid cached catalog should remain usable after stale capabilities are removed"
        );
        let sanitized: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(sanitized["models"][0].get("supports_search_tool").is_none());
        assert!(sanitized["models"][0].get("web_search_tool_type").is_none());
        let sanitized_bytes = fs::read(&path).unwrap();

        assert!(prepare_cached_catalog_for_native_web_search(home.path(), &[]).unwrap());
        assert_eq!(fs::read(&path).unwrap(), sanitized_bytes);
    }

    #[test]
    fn third_party_catalog_does_not_inherit_a_high_only_template() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_sol_reasoning_metadata(
            home.path(),
            Some(json!([{"effort": "high"}])),
            Some("high"),
        );
        let selected = vec!["provider-fast-coder".into()];

        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&selected), &selected,).unwrap(),
            1
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let model = &catalog["models"][0];
        assert_eq!(model["slug"], "provider-fast-coder");
        assert_eq!(
            reasoning_efforts_from_value(model),
            ["low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            model["default_reasoning_level"],
            THIRD_PARTY_DEFAULT_REASONING_EFFORT
        );
        for level in model["supported_reasoning_levels"].as_array().unwrap() {
            assert!(
                level_has_runtime_description(level),
                "{} level lacks the runtime-required description",
                level["effort"]
            );
        }
    }

    #[test]
    fn configured_provider_model_survives_a_missing_upstream_snapshot() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec!["provider-fast-coder".into()];

        assert_eq!(
            refresh_for_provider(home.path(), false, None, &selected,).unwrap(),
            OFFICIAL_MODELS.len() + 1
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        let model = models.last().unwrap();
        assert_eq!(model["slug"], "provider-fast-coder");
        assert_eq!(model["codey_source"], "third_party");
        assert_eq!(model["visibility"], "list");
        assert_eq!(model["supported_in_api"], true);
        assert_native_fast(model);
        assert!(
            !model["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn prompt_free_cold_start_falls_back_instead_of_writing_an_invalid_catalog() {
        let home = tempfile::tempdir().unwrap();

        let error = refresh_for_provider(home.path(), true, None, &[]).unwrap_err();

        assert!(error.to_string().contains("模型缓存缺少运行时必需字段"));
        assert!(is_runtime_model_cache_unavailable(&error));
        assert!(!home.path().join(MODEL_CATALOG_RELATIVE_PATH).exists());
        assert!(!is_available(home.path()));
        let state = selection_state(home.path(), true, None, &[], None).unwrap();
        assert_eq!(state.official_models.len(), OFFICIAL_MODELS.len());
        assert_eq!(state.available_model("gpt-6-astra"), Some("gpt-6-astra"));
    }

    #[test]
    fn astra_selection_and_route_metadata_follow_the_native_reasoning_levels() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec!["gpt-6-astra".into()];
        let expected_efforts = ["low", "medium", "high", "xhigh", "max", "ultra"];

        let state =
            selection_state(home.path(), true, None, &selected, Some("GPT-6-ASTRA")).unwrap();
        assert_eq!(state.default_model, "gpt-6-astra");
        assert_eq!(
            state.official_models[0].supported_reasoning_efforts,
            expected_efforts
        );
        assert_eq!(state.official_models[0].default_reasoning_effort, "medium");
        refresh_for_provider(home.path(), true, None, &selected).unwrap();
        let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
        assert_eq!(
            catalog["models"][0]["service_tiers"][0]["description"],
            "2x speed, increased usage"
        );

        let route = vec!["relay/GPT-6-ASTRA".into()];
        let state = selection_state(home.path(), false, Some(&route), &route, None).unwrap();
        let metadata = state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == route[0])
            .unwrap();
        assert_eq!(metadata.supported_reasoning_efforts, expected_efforts);

        let mut cache = official_cache();
        let astra = cache["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["slug"] == "gpt-6-astra")
            .unwrap();
        astra["supported_reasoning_levels"]
            .as_array_mut()
            .unwrap()
            .retain(|level| level["effort"] != "ultra");
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();

        refresh_for_provider(home.path(), false, Some(&route), &route).unwrap();
        let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
        let astra = &catalog["models"][0];
        assert_eq!(
            reasoning_efforts_from_value(astra),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert!(astra.get("multi_agent_version").is_none());
        assert!(astra.get("multi_agent_reasoning_effort").is_none());
    }

    #[test]
    fn cache_without_astra_keeps_existing_routes_and_requires_its_own_runtime_template() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = official_cache();
        cache["models"]
            .as_array_mut()
            .unwrap()
            .retain(|model| model["slug"] != "gpt-6-astra");
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();

        for (official, model) in [
            (true, "gpt-5.6-sol"),
            (false, "relay/gpt-5.6-sol"),
            (false, "provider/custom-model"),
        ] {
            let selected = vec![model.into()];
            assert_eq!(
                refresh_for_provider(home.path(), official, Some(&selected), &selected).unwrap(),
                1
            );
        }

        let path = home.path().join(relative_path());
        let previous = fs::read(&path).unwrap();
        for (official, model) in [(true, "gpt-6-astra"), (false, "relay/gpt-6-astra")] {
            let selected = vec![model.into()];
            let error = refresh_for_provider(home.path(), official, Some(&selected), &selected)
                .unwrap_err();
            assert!(is_runtime_model_cache_unavailable(&error));
            assert_eq!(fs::read(&path).unwrap(), previous);
        }
    }

    #[test]
    fn prompt_free_existing_catalog_is_not_reused_as_a_runtime_fallback() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&codey_runtime_core::model_suffix::bundled_model_catalog().unwrap())
                .unwrap(),
        )
        .unwrap();

        assert!(!is_available(home.path()));
    }

    #[test]
    fn existing_catalog_without_description_is_not_reused_as_a_runtime_fallback() {
        let home = tempfile::tempdir().unwrap();
        let mut catalog = official_cache();
        for model in catalog["models"].as_array_mut().unwrap() {
            model["description"] = json!(model["display_name"].as_str().unwrap_or("Model"));
        }
        catalog["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("description");
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();

        assert!(!is_available(home.path()));
    }

    #[test]
    fn startup_repair_fills_legacy_catalog_descriptions_once() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "models": [
                    {
                        "slug": "model-a",
                        "display_name": "Model A",
                        "base_instructions": "instructions",
                        "supported_reasoning_levels": [
                            { "effort": "low" },
                            { "effort": "high", "description": "Existing level" }
                        ]
                    },
                    {
                        "slug": "model-b",
                        "description": "   ",
                        "base_instructions": "instructions"
                    },
                    {
                        "slug": "model-c",
                        "display_name": "Model C",
                        "description": "Existing description",
                        "base_instructions": "instructions"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(repair_missing_descriptions(home.path()).unwrap());

        let repaired: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(repaired["models"][0]["description"], "Model A");
        assert_eq!(
            repaired["models"][0]["supported_reasoning_levels"][0]["description"],
            "Fast responses with lighter reasoning"
        );
        assert_eq!(
            repaired["models"][0]["supported_reasoning_levels"][1]["description"],
            "Existing level"
        );
        assert_eq!(repaired["models"][1]["description"], "model-b");
        assert_eq!(repaired["models"][2]["description"], "Existing description");
        assert!(is_available(home.path()));
        let repaired_contents = fs::read(&path).unwrap();

        assert!(!repair_missing_descriptions(home.path()).unwrap());
        assert_eq!(fs::read(&path).unwrap(), repaired_contents);
    }

    #[test]
    fn startup_repair_leaves_unrepairable_catalog_untouched() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = serde_json::to_vec(&json!({
            "models": [{
                "base_instructions": "instructions"
            }]
        }))
        .unwrap();
        fs::write(&path, &original).unwrap();

        let error = repair_missing_descriptions(home.path()).unwrap_err();

        assert!(error.to_string().contains("无法自动补全 description"));
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn cold_start_replaces_stale_codey_fast_metadata() {
        let home = tempfile::tempdir().unwrap();
        let mut stale_catalog = official_cache();
        for model in stale_catalog["models"].as_array_mut().unwrap() {
            add_fast_speed_controls(model);
        }
        let catalog_path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        fs::write(&catalog_path, serde_json::to_vec(&stale_catalog).unwrap()).unwrap();

        refresh_for_provider(home.path(), true, None, &[]).unwrap();

        let catalog: Value = serde_json::from_slice(&fs::read(catalog_path).unwrap()).unwrap();
        for slug in ["gpt-5.4-mini", "gpt-5.3-codex-spark"] {
            let model = catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|model| model["slug"] == slug)
                .unwrap();
            assert_no_native_fast(model);
        }
    }

    #[test]
    fn synced_empty_provider_catalog_hides_every_model() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = Vec::<String>::new();
        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&upstream), &[],).unwrap(),
            0
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog["models"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn unsynced_third_party_provider_does_not_invent_official_models() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());

        let state = selection_state(home.path(), false, None, &[], None).unwrap();

        assert!(state.official_models.is_empty());
        assert!(state.third_party_models.is_empty());
        assert!(state.default_model.is_empty());
    }

    #[test]
    fn synced_third_party_provider_keeps_official_looking_ids_route_scoped() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec!["gpt-5.6-sol".into(), "third-model".into()];
        let state = selection_state_with_manual_models(
            home.path(),
            false,
            Some(&upstream),
            &["gpt-5.6-sol".into(), "third-model".into()],
            &["third-model".into()],
            None,
        )
        .unwrap();

        assert!(state.official_models.is_empty());
        assert_eq!(state.third_party_models, ["gpt-5.6-sol", "third-model"]);
        let sol_metadata = state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(
            sol_metadata.supported_reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        let custom_metadata = state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == "third-model")
            .unwrap();
        assert_eq!(
            custom_metadata.supported_reasoning_efforts,
            ["low", "medium", "high", "xhigh"]
        );
        assert_eq!(state.manual_third_party_models, ["third-model"]);
        assert_eq!(state.default_model, "gpt-5.6-sol");
    }

    #[test]
    fn synced_empty_provider_marks_official_models_and_configured_models_unavailable() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = Vec::new();

        let state = selection_state(
            home.path(),
            false,
            Some(&upstream),
            &["provider-fast-coder".into()],
            None,
        )
        .unwrap();

        assert!(state.official_models.is_empty());
        assert!(state.third_party_models.is_empty());
        assert!(state.upstream_models.is_empty());
        assert!(state.default_model.is_empty());
    }

    #[test]
    fn selection_state_does_not_enable_unselected_upstream_models() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec!["gpt-5.4".into(), "codex-auto-review".into()];
        let state = selection_state(home.path(), false, Some(&upstream), &[], None).unwrap();

        assert!(state.official_models.is_empty());
        assert!(state.third_party_models.is_empty());
        assert!(state.default_model.is_empty());
    }

    #[test]
    fn synced_provider_keeps_selected_spark_as_a_route_model() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec!["gpt-5.3-codex-spark".into()];

        let state = selection_state(
            home.path(),
            false,
            Some(&upstream),
            &["gpt-5.3-codex-spark".into()],
            None,
        )
        .unwrap();

        assert!(state.official_models.is_empty());
        assert_eq!(state.third_party_models, ["gpt-5.3-codex-spark"]);
        let spark_metadata = state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == "gpt-5.3-codex-spark")
            .unwrap();
        assert_eq!(
            spark_metadata.supported_reasoning_efforts,
            ["low", "medium", "high", "xhigh"]
        );
        let sol_metadata = state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(
            sol_metadata.supported_reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(state.default_model, "gpt-5.3-codex-spark");
    }

    #[test]
    fn selection_state_uses_requested_default_when_available() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec![
            "GPT-5.6-SOL".into(),
            "gpt-5.6-luna".into(),
            "Third-Model".into(),
        ];
        let state = selection_state(
            home.path(),
            false,
            Some(&upstream),
            &[
                "third-model".into(),
                "THIRD-MODEL".into(),
                "gpt-5.6-luna".into(),
            ],
            Some("THIRD-MODEL"),
        )
        .unwrap();

        assert_eq!(state.default_model, "third-model");
        assert_eq!(
            state.available_model(" GPT-5.6-LUNA "),
            Some("gpt-5.6-luna")
        );
        assert_eq!(state.available_model("third-model"), Some("third-model"));
        assert_eq!(state.available_model("gpt-5.4"), None);
        assert!(state.official_models.is_empty());
    }

    #[test]
    fn official_selection_marks_only_enabled_models_as_supported() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec!["gpt-5.6-sol".into()];

        let state =
            selection_state(home.path(), true, None, &selected, Some("gpt-5.6-luna")).unwrap();

        assert_eq!(state.default_model, "gpt-5.6-sol");
        assert!(
            state
                .official_models
                .iter()
                .find(|model| model.slug == "gpt-5.6-sol")
                .is_some_and(|model| model.supported)
        );
        assert!(
            state
                .official_models
                .iter()
                .filter(|model| model.slug != "gpt-5.6-sol")
                .all(|model| !model.supported)
        );
    }

    #[test]
    fn selection_state_falls_back_from_unavailable_default_to_first_route_model() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec!["gpt-5.6-sol".into(), "third-model".into()];
        let state = selection_state(
            home.path(),
            false,
            Some(&upstream),
            &["third-model".into()],
            Some("missing-model"),
        )
        .unwrap();

        assert_eq!(state.default_model, "third-model");
    }

    #[test]
    fn selection_exposes_every_model_available_on_the_current_route() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&official_cache()).unwrap(),
        )
        .unwrap();
        let upstream = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.6-terra".into(),
            "gpt-5.6-luna".into(),
            "third-model".into(),
        ];

        let state = selection_state(home.path(), false, Some(&upstream), &upstream, None).unwrap();

        for model in &upstream {
            assert!(state.available_model(model).is_some(), "{model}");
        }
        assert_eq!(state.available_model("gpt-5.4"), None);
    }

    #[test]
    fn selection_ignores_a_stale_shared_cache_client_version() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = official_cache();
        cache["client_version"] = json!("0.146.1");
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
        let upstream = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.6-luna".into(),
            "third-model".into(),
        ];

        let state = selection_state(
            home.path(),
            false,
            Some(&upstream),
            &["gpt-5.6-luna".into(), "third-model".into()],
            None,
        )
        .unwrap();

        assert_eq!(state.available_model("gpt-5.6-luna"), Some("gpt-5.6-luna"));
        assert_eq!(state.available_model("third-model"), Some("third-model"));
    }

    #[test]
    fn selection_does_not_depend_on_a_generated_runtime_catalog() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());

        let state = selection_state(home.path(), true, None, &[], None).unwrap();

        assert!(!home.path().join(relative_path()).exists());
        for model in &state.official_models {
            assert_eq!(
                state.available_model(&model.slug),
                Some(model.slug.as_str())
            );
        }
        assert!(state.first_available_model().is_some());
    }

    #[test]
    fn catalog_snapshot_restores_existing_content_and_removes_new_content() {
        let existing_home = tempfile::tempdir().unwrap();
        let existing_path = existing_home.path().join(relative_path());
        fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
        fs::write(&existing_path, b"original catalog\n").unwrap();
        let existing_snapshot = snapshot(existing_home.path()).unwrap();
        fs::write(&existing_path, b"replacement catalog\n").unwrap();

        restore_snapshot(existing_snapshot).unwrap();

        assert_eq!(fs::read(&existing_path).unwrap(), b"original catalog\n");

        let new_home = tempfile::tempdir().unwrap();
        let new_path = new_home.path().join(relative_path());
        let new_snapshot = snapshot(new_home.path()).unwrap();
        fs::create_dir_all(new_path.parent().unwrap()).unwrap();
        fs::write(&new_path, b"new catalog\n").unwrap();

        restore_snapshot(new_snapshot).unwrap();

        assert!(!new_path.exists());
    }
}
