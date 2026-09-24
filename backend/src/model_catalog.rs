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
/// Raw `codex debug models` output Codey captured itself. Recent Codex builds
/// no longer maintain `models_cache.json` on disk, so this snapshot is the
/// durable source for models that need instruction-bearing entries.
const DEBUG_CATALOG_RELATIVE_PATH: &str = "model-catalogs/codey-runtime-catalog.json";
/// Newest Codex CLI build whose catalog Codey already rendered in this process.
/// The render takes seconds, so the same build is not consulted twice, while a
/// Codex upgrade replaces the binary and its modification time and therefore
/// lets a restart inside the same Codey process capture the new catalog.
#[cfg(not(test))]
static RUNTIME_SNAPSHOT_SYNC_ATTEMPTED: std::sync::Mutex<Option<std::time::SystemTime>> =
    std::sync::Mutex::new(None);
/// Context window an older Codey version wrote for models flagged as 1M capable.
const LEGACY_1M_CONTEXT_WINDOW: u64 = 1_000_000;
const DEFAULT_CONTEXT_WINDOW: u64 = 272_000;
const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u64 = 95;
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
/// Official account models Codey exposes. Upstream retires a model by dropping
/// it from the Codex model cache, so a retired slug has to leave this list in
/// the same change; otherwise the picker keeps offering a model the account can
/// no longer call.
const OFFICIAL_MODELS: [(&str, &str); 5] = [
    ("gpt-6-astra", "GPT-6-Astra"),
    ("gpt-5.6-sol", "GPT-5.6-Sol"),
    ("gpt-5.6-terra", "GPT-5.6-Terra"),
    ("gpt-5.6-luna", "GPT-5.6-Luna"),
    ("gpt-5.5", "GPT-5.5"),
];

#[derive(Debug)]
struct RuntimeModelCacheUnavailable;

pub(crate) const CUSTOM_CONTEXT_CATALOG_UNAVAILABLE: &str =
    "无法生成带有自定义上下文预算的模型目录，请恢复默认预算或重新同步模型";

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
    /// Values declared to Codex, after the user's declaration is applied.
    pub supported_reasoning_efforts: Vec<String>,
    /// Values the official template or the fallback list would declare.
    pub auto_supported_reasoning_efforts: Vec<String>,
    /// Explicit user declaration; empty when the model follows the template.
    pub reasoning_efforts: Vec<crate::config::ModelReasoningEffort>,
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
        None => crate::fs_util::remove_file_if_exists(&snapshot.path)
            .with_context(|| format!("移除新建的 Codey 模型目录失败：{}", snapshot.path.display())),
    }
}

pub fn default_official_model_slugs() -> Vec<String> {
    OFFICIAL_MODELS
        .iter()
        .map(|(slug, _)| (*slug).to_string())
        .collect()
}

/// 本次刷新声明的模型能力清单；为 `None` 表示不声明该类能力。
#[derive(Clone, Copy, Default)]
pub(crate) struct CapabilityLists<'a> {
    pub(crate) websocket_models: Option<&'a [String]>,
    pub(crate) native_web_search_models: Option<&'a [String]>,
    pub(crate) image_detail_original_models: Option<&'a [String]>,
}

/// 生成目录时一并应用的上下文与思考等级覆盖。
#[derive(Clone, Copy)]
pub(crate) struct CatalogOverrides<'a> {
    pub(crate) contexts: &'a std::collections::BTreeMap<String, crate::config::ModelContextConfig>,
    pub(crate) reasoning_efforts:
        &'a std::collections::BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>,
}

#[cfg(test)]
pub fn refresh_for_provider(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
) -> Result<usize> {
    refresh_for_provider_with_capabilities(
        home,
        official_provider,
        upstream_models,
        selected_models,
        CapabilityLists::default(),
        "",
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
    refresh_for_provider_with_capabilities(
        home,
        official_provider,
        upstream_models,
        selected_models,
        CapabilityLists {
            websocket_models: Some(websocket_models),
            ..CapabilityLists::default()
        },
        "",
    )
}

#[cfg(test)]
pub(crate) fn refresh_for_provider_with_capabilities(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    capabilities: CapabilityLists<'_>,
    codex_app_path: &str,
) -> Result<usize> {
    let models = render_catalog_for_provider(
        home,
        official_provider,
        upstream_models,
        selected_models,
        capabilities,
        codex_app_path,
    )?;
    write_verified_catalog(home, &models)
}

pub(crate) fn refresh_for_provider_with_contexts(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    capabilities: CapabilityLists<'_>,
    overrides: CatalogOverrides<'_>,
    codex_app_path: &str,
) -> Result<usize> {
    let mut models = render_catalog_for_provider(
        home,
        official_provider,
        upstream_models,
        selected_models,
        capabilities,
        codex_app_path,
    )?;
    apply_overrides_to_models(&mut models, overrides)?;
    write_verified_catalog(home, &models)
}

pub(crate) fn apply_catalog_overrides(home: &Path, overrides: CatalogOverrides<'_>) -> Result<()> {
    let mut models = read_runtime_catalog_models(home)?;
    apply_overrides_to_models(&mut models, overrides)?;
    write_verified_catalog(home, &models)?;
    Ok(())
}

fn apply_overrides_to_models(models: &mut [Value], overrides: CatalogOverrides<'_>) -> Result<()> {
    for model in models {
        let policy = model.get("slug").and_then(Value::as_str).and_then(|slug| {
            overrides
                .contexts
                .iter()
                .find(|(key, _)| model_id::equal(key, slug))
                .map(|(_, policy)| policy)
        });
        apply_model_context(model, policy)?;
        let declaration = model.get("slug").and_then(Value::as_str).and_then(|slug| {
            overrides
                .reasoning_efforts
                .iter()
                .find(|(key, _)| model_id::equal(key, slug))
                .map(|(_, efforts)| efforts.as_slice())
        });
        apply_model_reasoning_efforts(model, declaration);
    }
    Ok(())
}

pub(crate) fn runtime_context_metadata(
    home: &Path,
) -> std::collections::BTreeMap<String, serde_json::Map<String, Value>> {
    read_runtime_catalog_models(home)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|model| {
            let slug = model["slug"].as_str()?.to_string();
            let fields = [
                "context_window",
                "max_context_window",
                "effective_context_window_percent",
                "auto_compact_token_limit",
                "codey_context_source",
            ];
            Some((
                slug,
                fields
                    .into_iter()
                    .map(|field| (field.to_string(), model[field].clone()))
                    .collect(),
            ))
        })
        .collect()
}

pub(crate) fn apply_model_context(
    model: &mut Value,
    policy: Option<&crate::config::ModelContextConfig>,
) -> Result<()> {
    let Some(policy) = policy else {
        return Ok(());
    };
    policy.validate().map_err(anyhow::Error::msg)?;
    let window = policy.context_window_tokens;
    let percent = (window - policy.reserve_output_tokens.unwrap_or(0)) * 100 / window;
    model["context_window"] = json!(window);
    model["max_context_window"] = json!(window);
    model["effective_context_window_percent"] = json!(percent);
    model["auto_compact_token_limit"] = json!(
        policy
            .auto_compact_token_limit
            .unwrap_or((window * 9 / 10).min(window * percent / 100))
    );
    model["codey_context_source"] = json!("user_declared");
    Ok(())
}

/// Shadow copy of the declaration an override replaced, so dropping the
/// override restores the template values instead of keeping stale ones.
const REASONING_BASE_FIELD: &str = "codey_reasoning_base";
const REASONING_DECLARATION_FIELDS: [&str; 3] = [
    "supported_reasoning_levels",
    "default_reasoning_level",
    "supports_reasoning_summaries",
];

#[cfg(test)]
fn apply_catalog_reasoning_efforts(
    home: &Path,
    overrides: &std::collections::BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>,
) -> Result<()> {
    let mut models = read_runtime_catalog_models(home)?;
    for model in &mut models {
        let declaration = model.get("slug").and_then(Value::as_str).and_then(|slug| {
            overrides
                .iter()
                .find(|(key, _)| model_id::equal(key, slug))
                .map(|(_, efforts)| efforts.as_slice())
        });
        apply_model_reasoning_efforts(model, declaration);
    }
    write_verified_catalog(home, &models)?;
    Ok(())
}

pub(crate) fn apply_model_reasoning_efforts(
    model: &mut Value,
    declaration: Option<&[crate::config::ModelReasoningEffort]>,
) {
    if declaration.is_none() && model.get(REASONING_BASE_FIELD).is_none() {
        return;
    }
    if model.get(REASONING_BASE_FIELD).is_none() {
        let mut base = serde_json::Map::new();
        for field in REASONING_DECLARATION_FIELDS {
            if let Some(value) = model.get(field) {
                base.insert(field.to_string(), value.clone());
            }
        }
        model[REASONING_BASE_FIELD] = Value::Object(base);
    }
    let base = model.get(REASONING_BASE_FIELD).cloned().unwrap_or_default();
    let Some(declaration) = declaration else {
        for field in REASONING_DECLARATION_FIELDS {
            match base.get(field) {
                Some(value) => model[field] = value.clone(),
                None => {
                    if let Some(object) = model.as_object_mut() {
                        object.remove(field);
                    }
                }
            }
        }
        if let Some(object) = model.as_object_mut() {
            object.remove(REASONING_BASE_FIELD);
        }
        return;
    };
    let base_levels = base
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let description_for = |level: &str, value: &str| {
        base_levels
            .iter()
            .find(|entry| {
                entry.get("effort").and_then(Value::as_str) == Some(level)
                    || entry.get("effort").and_then(Value::as_str) == Some(value)
            })
            .and_then(|entry| entry.get("description"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| reasoning_level_description(value))
    };
    let levels = declaration
        .iter()
        .map(|effort| {
            json!({
                "effort": effort.value,
                "description": description_for(&effort.level, &effort.value),
            })
        })
        .collect::<Vec<_>>();
    let default = base
        .get("default_reasoning_level")
        .and_then(Value::as_str)
        .filter(|default| declaration.iter().any(|effort| effort.value == *default))
        .map(ToString::to_string)
        .or_else(|| {
            declaration
                .iter()
                .find(|effort| effort.level == THIRD_PARTY_DEFAULT_REASONING_EFFORT)
                .map(|effort| effort.value.clone())
        })
        .or_else(|| declaration.first().map(|effort| effort.value.clone()))
        .unwrap_or_else(|| THIRD_PARTY_DEFAULT_REASONING_EFFORT.to_string());
    model["supported_reasoning_levels"] = Value::Array(levels);
    model["default_reasoning_level"] = json!(default);
    model["supports_reasoning_summaries"] = json!(!declaration.is_empty());
}

fn render_catalog_for_provider(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    capabilities: CapabilityLists<'_>,
    codex_app_path: &str,
) -> Result<Vec<Value>> {
    let CapabilityLists {
        websocket_models,
        native_web_search_models,
        image_detail_original_models,
    } = capabilities;
    if !official_provider
        && upstream_models.is_some_and(|models| models.is_empty())
        && selected_models.is_empty()
    {
        return Ok(Vec::new());
    }
    let mut official_models = read_official_entries(home)?;
    // Codex 26.908+ may never write `models_cache.json`; capture the CLI's own
    // catalog before declaring the runtime cache unusable. A snapshot older
    // than the local Codex binary is refreshed again so a
    // Codex upgrade does not keep serving stale instructions.
    let sources_unusable = official_models
        .iter()
        .all(|model| model_instruction_source(model).is_none());
    let stale_snapshot = !sources_unusable && runtime_snapshot_is_stale(home, codex_app_path);
    if sources_unusable || stale_snapshot {
        let snapshot_synced =
            sync_runtime_catalog_snapshot(home, codex_app_path).unwrap_or_else(|error| {
                eprintln!("读取 Codex 命令行模型清单失败：{error:#}");
                false
            });
        if snapshot_synced {
            official_models = read_official_entries(home)?;
        }
    }
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
                || slug.is_some_and(|slug| {
                    let key = model_id::key(slug);
                    selected_model_keys.contains(&key) || upstream.contains(&key)
                })
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
        // Mixed catalogs must keep compatible official raw slugs for spawn_agent,
        // but a newly bundled official model without a local runtime template
        // cannot fail the whole refresh. Official-only generation still
        // fail-closes on that slug, and so does a user-declared third-party
        // route that reuses an official model id.
        catalog_models.retain(model_is_runtime_source_compatible);
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
            // A derived official route only mirrors the model list of its own
            // account. A model the local Codex cache no longer carries is
            // dropped like the raw official entry above instead of falling back
            // to the generic template or blocking every route, while a
            // third-party alias still fails closed on the final check.
            if is_official_route_alias(model_id)
                && source_template
                    .is_none_or(|template| model_instruction_source(template).is_none())
            {
                continue;
            }
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
    // 目录顺序跟随线路配置里的模型顺序，Codex 原生列表与 Codey 分组菜单才一致。
    model_id::sort_by_selection_order(&mut catalog_models, selected_models, |model| {
        model.get("slug").and_then(Value::as_str)
    });
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
    if let Some(image_detail_original_models) = image_detail_original_models {
        let image_detail_original_model_keys = image_detail_original_models
            .iter()
            .map(|model| model_id::key(model))
            .collect::<HashSet<_>>();
        for model in &mut catalog_models {
            gate_cached_image_detail_original(model, &image_detail_original_model_keys);
        }
    }
    for model in &mut catalog_models {
        prepare_cached_context_window(model);
        // Codex 只发送模型目录里声明过的思考强度。目录没有 ultra 时，界面仍能
        // 选中它，请求会被静默改成该模型的默认档（luna 是 medium）。
        ensure_forwarded_ultra_level(model);
    }
    // Third-party routes still fail closed when their template lacks runtime
    // fields. Official-only catalogs never drop incompatible slugs above, so
    // this remains all-or-nothing for that path.
    if !catalog_models.is_empty() {
        ensure_runtime_compatible_models(&catalog_models)?;
    }
    Ok(catalog_models)
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
        None,
        requested_default_model,
    )
}

pub fn selection_state_with_manual_models(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    manual_third_party_models: &[String],
    reasoning_efforts: Option<
        &std::collections::BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>,
    >,
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
    let requested_upstream_keys =
        upstream_models
            .filter(|models| !models.is_empty())
            .map(|models| {
                models
                    .iter()
                    .map(|model| model_id::key(model))
                    .collect::<HashSet<_>>()
            });
    let official_model_ids = official_entries
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .filter(|slug| {
            requested_upstream_keys
                .as_ref()
                .is_none_or(|keys| keys.contains(&model_id::key(slug)))
        })
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let selected_official_keys = selected_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let filter_official_selection = official_provider
        && (!selected_official_keys.is_empty() || requested_upstream_keys.is_some());
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
                let slug = model.get("slug").and_then(Value::as_str).unwrap_or("");
                let supported_reasoning_efforts =
                    with_forwarded_ultra(slug, reasoning_efforts_from_value(model));
                let default_reasoning_effort =
                    default_reasoning_effort_from_value(model, &supported_reasoning_efforts);
                let model = official_model_from_value(model)?;
                let model_key = model_id::key(&model.slug);
                // 同步结果只说明账号能调用哪些模型。用户勾选的子集才是启用
                // 集合；有勾选时，未勾选的同步模型不能再进入运行时列表。
                let known_to_account = requested_upstream_keys
                    .as_ref()
                    .is_none_or(|keys| keys.contains(&model_key));
                let supported = if !filter_official_selection {
                    true
                } else if selected_official_keys.is_empty() {
                    requested_upstream_keys
                        .as_ref()
                        .is_some_and(|keys| keys.contains(&model_key))
                } else {
                    selected_official_keys.contains(&model_key) && known_to_account
                };
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
        third_party_model_metadata_from_entries(
            &official_entries,
            &third_party_models,
            reasoning_efforts,
        )
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

/// 角色的模型必须存在于实际交给 Codex 的目录，不能仅凭线路配置推断可用。
pub(crate) fn validate_runtime_subagent_models(
    catalog_path: &Path,
    roles: &std::collections::BTreeMap<String, crate::config::SubagentRoleConfig>,
) -> Result<()> {
    let models = read_runtime_catalog_models_at(catalog_path)?;
    for (role, selection) in roles.iter().filter(|(_, selection)| selection.enabled) {
        anyhow::ensure!(
            models.iter().any(|model| model["slug"]
                .as_str()
                .is_some_and(|slug| model_id::equal(slug, &selection.model))),
            "子代理角色 {role} 的模型 {} 未包含在本次 Codex 模型目录中；缓存目录可能已过期，请重新同步模型并重启 Codex",
            selection.model
        );
    }
    Ok(())
}

/// Makes a previously generated catalog safe to reuse when the upstream model
/// cache cannot be refreshed. Older catalogs may still advertise capabilities
/// whose route settings have since changed.
pub(crate) fn prepare_cached_catalog_for_current_capabilities(
    home: &Path,
    native_web_search_models: &[String],
    image_detail_original_models: &[String],
) -> Result<bool> {
    if !is_available(home) {
        return Ok(false);
    }

    let mut models = read_runtime_catalog_models(home)?;
    let allowed_model_keys = native_web_search_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let image_detail_original_model_keys = image_detail_original_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let mut changed = false;
    for model in &mut models {
        let previous = model.clone();
        gate_cached_native_web_search(model, &allowed_model_keys);
        gate_cached_image_detail_original(model, &image_detail_original_model_keys);
        prepare_cached_context_window(model);
        changed |= *model != previous;
    }
    if changed {
        write_catalog(home, &models)?;
    }

    let written_models = read_runtime_catalog_models(home)?;
    let mut safely_gated_models = written_models.clone();
    for model in &mut safely_gated_models {
        gate_cached_native_web_search(model, &allowed_model_keys);
        gate_cached_image_detail_original(model, &image_detail_original_model_keys);
        prepare_cached_context_window(model);
    }
    if safely_gated_models != written_models {
        bail!("复用的 Codey 模型目录仍包含与当前线路不匹配的模型能力");
    }
    Ok(true)
}

/// Repairs catalogs written by older Codey versions that copied model-cache
/// entries without Codex's now-required `description` fields on models and
/// their reasoning levels.
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
    let paths = ordered_catalog_sources(home);
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

/// Raw catalog sources, newest first. A snapshot is captured only when it is
/// missing or older than the installed Codex, so whenever it is newer than
/// `models_cache.json` it carries instructions the cache cannot: without this
/// ordering a Codex upgrade that stops maintaining the cache would keep the
/// pre-upgrade instructions forever, and the refresh would be wasted work.
/// Missing files sort last; they contribute nothing either way.
fn ordered_catalog_sources(home: &Path) -> Vec<PathBuf> {
    let mut raw = [
        home.join("models_cache.json"),
        home.join(DEBUG_CATALOG_RELATIVE_PATH),
    ];
    raw.sort_by_key(|path| {
        std::cmp::Reverse(
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok(),
        )
    });
    raw.into_iter()
        .chain([home.join(relative_path())])
        .collect()
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

    // A Codey-owned CLI snapshot is account-backed and therefore the source
    // of truth. Keeping the old fixed list below is still useful for first-run
    // and offline fallback, but it must not hide newly released models that
    // the signed-in account can actually use. A generic models_cache.json is
    // intentionally not treated as dynamic input because older Codex builds
    // can leave retired models in that file.
    let dynamic_source = paths
        .iter()
        .position(|path| path.ends_with(DEBUG_CATALOG_RELATIVE_PATH))
        .and_then(|index| {
            fs::read(&paths[index])
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .filter(|value| {
                    value
                        .get("codey_account_snapshot")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|value| official_models_from_value(&value))
        });
    // Account `/models` payloads name the slugs the signed-in account can call,
    // but they omit the instruction templates Codex needs to launch a model.
    // Fill those from the local cache before deciding the snapshot is unusable,
    // so a newly released slug is not dropped back to the fixed fallback list.
    let dynamic_source = dynamic_source.map(|models| {
        let templates = catalogs.iter().flatten().cloned().collect::<Vec<_>>();
        complete_account_models(&models, &templates)
    });
    if let Some(source) = dynamic_source
        .as_deref()
        .filter(|models| snapshot_has_runtime_models(models))
    {
        let mut seen = HashSet::new();
        let models = source
            .iter()
            .filter(|model| {
                model_instruction_source(model).is_some()
                    && model
                        .get("visibility")
                        .and_then(Value::as_str)
                        .is_none_or(|visibility| visibility != "hide")
            })
            .filter_map(|model| {
                let slug = model.get("slug").and_then(Value::as_str)?.trim();
                if slug.is_empty() || !seen.insert(model_id::key(slug)) {
                    return None;
                }
                let mut model = model.clone();
                let display_name = model
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(slug)
                    .to_string();
                normalize_official_model(&mut model, slug, &display_name, seen.len() - 1);
                Some(model)
            })
            .collect::<Vec<_>>();
        if !models.is_empty() {
            return Ok(models);
        }
    }

    OFFICIAL_MODELS
        .iter()
        .enumerate()
        .map(|(priority, (slug, display_name))| {
            let matching_models = catalogs
                .iter()
                .flat_map(|models| models.iter())
                .filter(|model| model.get("slug").and_then(Value::as_str) == Some(*slug))
                .collect::<Vec<_>>();
            // A source may hold the slug without runtime fields (for example an
            // incomplete `models_cache.json` written before a Codex update).
            // Prefer the first instruction-bearing entry so a fresher Codey
            // snapshot can repair it instead of failing the whole catalog.
            let mut model = matching_models
                .iter()
                .find(|model| model_instruction_source(model).is_some())
                .or_else(|| matching_models.first())
                .map(|model| (*model).clone())
                .ok_or_else(|| anyhow::anyhow!("Codex 模型模板缺少固定官方模型 {slug}"))?;
            let fallbacks = matching_models;
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

/// The same install keeps different Codex builds in its staged and app
/// directories, so the stamp covers every candidate: a newer build anywhere is
/// enough to justify re-rendering the catalog once.
fn codex_cli_stamp_for(candidates: &[PathBuf]) -> Option<std::time::SystemTime> {
    candidates
        .iter()
        .filter_map(|path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .max()
}

/// The snapshot is missing or predates the local Codex CLI. A missing snapshot
/// counts as stale because a Codex upgrade that arrives without a
/// `models_cache.json` has no other way to produce one, while an existing
/// instruction-bearing source would otherwise suppress the capture.
#[cfg(not(test))]
fn runtime_snapshot_is_stale(home: &Path, codex_app_path: &str) -> bool {
    let snapshot = home.join(DEBUG_CATALOG_RELATIVE_PATH);
    let snapshot_is_account_backed = read_catalog_value(&snapshot)
        .and_then(|value| value.get("codey_account_snapshot").cloned())
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !snapshot_is_account_backed {
        return true;
    }
    let snapshot_mtime = fs::metadata(home.join(DEBUG_CATALOG_RELATIVE_PATH))
        .and_then(|metadata| metadata.modified())
        .ok();
    runtime_snapshot_is_stale_at(
        snapshot_mtime,
        codex_cli_stamp_for(&codex_cli_candidates_for(codex_app_path)),
    )
}

fn runtime_snapshot_is_stale_at(
    snapshot_mtime: Option<std::time::SystemTime>,
    cli_stamp: Option<std::time::SystemTime>,
) -> bool {
    let Some(cli_stamp) = cli_stamp else {
        return false;
    };
    snapshot_mtime.is_none_or(|snapshot_mtime| cli_stamp > snapshot_mtime)
}

/// Unit tests never run the Codex CLI installed on the build machine, and the
/// snapshot capture itself is stubbed there. Staleness stays reachable through
/// `runtime_snapshot_is_stale_at` so the decision logic can still be tested.
#[cfg(test)]
fn runtime_snapshot_is_stale(_home: &Path, _codex_app_path: &str) -> bool {
    false
}

/// Claims the right to render the catalog for the current local Codex build.
/// The claim is released again when the render fails so a later launch in the
/// same process can retry.
fn claim_snapshot_sync_slot(
    attempted: &std::sync::Mutex<Option<std::time::SystemTime>>,
    stamp: Option<std::time::SystemTime>,
) -> bool {
    let Ok(mut attempted) = attempted.lock() else {
        return true;
    };
    if stamp.is_some() && *attempted == stamp {
        return false;
    }
    *attempted = stamp;
    true
}

fn release_snapshot_sync_slot(
    attempted: &std::sync::Mutex<Option<std::time::SystemTime>>,
    stamp: Option<std::time::SystemTime>,
) {
    if stamp.is_none() {
        return;
    }
    if let Ok(mut attempted) = attempted.lock()
        && *attempted == stamp
    {
        *attempted = None;
    }
}

/// Codex 26.908.x stopped maintaining `models_cache.json` on disk. When no
/// file source carries instruction-bearing entries, ask the bundled Codex CLI
/// to render its catalog and keep the output as a Codey-owned snapshot that
/// `read_official_entries` picks up like any other source.
#[cfg(not(test))]
fn sync_runtime_catalog_snapshot(home: &Path, codex_app_path: &str) -> Result<bool> {
    let candidates = codex_cli_candidates_for(codex_app_path);
    let stamp = codex_cli_stamp_for(&candidates);
    if !claim_snapshot_sync_slot(&RUNTIME_SNAPSHOT_SYNC_ATTEMPTED, stamp) {
        return Ok(false);
    }
    let Some(models) = debug_model_entries(home, &candidates) else {
        eprintln!("本机 Codex 命令行未产出可用的模型清单快照");
        // Nothing was captured, so a later launch in this process may retry.
        release_snapshot_sync_slot(&RUNTIME_SNAPSHOT_SYNC_ATTEMPTED, stamp);
        return Ok(false);
    };
    let catalog = serde_json::to_vec_pretty(&json!({
        "models": models,
        "codey_account_snapshot": true,
    }))
    .context("序列化 Codex 内置模型目录快照失败")?;
    atomic_write(&home.join(DEBUG_CATALOG_RELATIVE_PATH), &catalog)
        .context("写入 Codex 内置模型目录快照失败")?;
    Ok(true)
}

/// Explicit model synchronization must be allowed to observe account changes
/// even when the installed Codex binary has not changed since the last launch.
pub(crate) fn refresh_account_runtime_snapshot(home: &Path, codex_app_path: &str) -> Result<bool> {
    #[cfg(not(test))]
    {
        if let Ok(mut attempted) = RUNTIME_SNAPSHOT_SYNC_ATTEMPTED.lock() {
            *attempted = None;
        }
    }
    sync_runtime_catalog_snapshot(home, codex_app_path)
}

/// Merges the account-specific model response into the Codey-owned runtime
/// catalog. The catalog is shared, while each route keeps its own upstream
/// model IDs and filters this union when building its selection state.
///
/// The account response is the availability list. It usually has no Codex
/// instruction template, so an existing runtime entry keeps those fields, and
/// a slug the local catalog has never seen borrows the closest template.
pub(crate) fn merge_account_runtime_models(home: &Path, models: &[Value]) -> Result<()> {
    let path = home.join(DEBUG_CATALOG_RELATIVE_PATH);
    let mut catalog = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or_else(|| json!({"models": [], "codey_account_snapshot": true}));
    let entries = catalog
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("Codex 模型目录快照格式无效"))?;
    let mut templates = cached_instruction_templates(home);
    templates.extend(entries.iter().cloned());
    let mut positions = entries
        .iter()
        .enumerate()
        .filter_map(|(index, model)| {
            model
                .get("slug")
                .and_then(Value::as_str)
                .map(|slug| (model_id::key(slug), index))
        })
        .collect::<std::collections::HashMap<_, _>>();
    for model in models.iter().filter(|model| {
        model
            .get("slug")
            .and_then(Value::as_str)
            .is_some_and(|slug| !slug.trim().is_empty())
    }) {
        let key = model_id::key(model["slug"].as_str().unwrap_or_default());
        let existing = positions
            .get(&key)
            .copied()
            .map(|index| entries[index].clone());
        let merged = merge_account_model(existing.as_ref(), model, &templates);
        if model_instruction_source(&merged).is_some() {
            templates.push(merged.clone());
        }
        if let Some(index) = positions.get(&key).copied() {
            entries[index] = merged;
        } else {
            positions.insert(key, entries.len());
            entries.push(merged);
        }
    }
    catalog["codey_account_snapshot"] = Value::Bool(true);
    let bytes = serde_json::to_vec_pretty(&catalog).context("序列化官方模型目录快照失败")?;
    atomic_write(&path, &bytes).context("写入官方模型目录快照失败")
}

fn cached_instruction_templates(home: &Path) -> Vec<Value> {
    fs::read(home.join("models_cache.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|value| official_models_from_value(&value))
        .unwrap_or_default()
        .into_iter()
        .filter(|model| model_instruction_source(model).is_some())
        .collect()
}

fn complete_account_models(models: &[Value], templates: &[Value]) -> Vec<Value> {
    models
        .iter()
        .map(|model| {
            if model_instruction_source(model).is_some() || model_is_hidden(model) {
                return model.clone();
            }
            let slug = model.get("slug").and_then(Value::as_str).unwrap_or("");
            instruction_template_for(slug, templates)
                .map(|template| account_model_with_runtime_template(template, model))
                .unwrap_or_else(|| model.clone())
        })
        .collect()
}

fn merge_account_model(existing: Option<&Value>, incoming: &Value, templates: &[Value]) -> Value {
    if model_instruction_source(incoming).is_some() {
        return existing
            .map(|existing| account_model_with_runtime_template(existing, incoming))
            .unwrap_or_else(|| incoming.clone());
    }
    if let Some(existing) = existing.filter(|model| model_instruction_source(model).is_some()) {
        return account_model_with_runtime_template(existing, incoming);
    }
    let slug = incoming.get("slug").and_then(Value::as_str).unwrap_or("");
    if let Some(template) = instruction_template_for(slug, templates) {
        return account_model_with_runtime_template(template, incoming);
    }
    existing.cloned().unwrap_or_else(|| incoming.clone())
}

/// `gpt-6-sol` and `gpt-5.6-sol` share the trailing family token.
fn model_family_key(slug: &str) -> &str {
    slug.rsplit_once('-')
        .map(|(_, family)| family)
        .unwrap_or(slug)
}

fn model_is_hidden(model: &Value) -> bool {
    model.get("visibility").and_then(Value::as_str) == Some("hide")
}

fn instruction_template_for<'a>(slug: &str, templates: &'a [Value]) -> Option<&'a Value> {
    let same_slug = templates.iter().find(|model| {
        model
            .get("slug")
            .and_then(Value::as_str)
            .is_some_and(|candidate| model_id::equal(candidate, slug))
            && model_instruction_source(model).is_some()
    });
    if let Some(model) = same_slug {
        return Some(model);
    }
    let family = model_family_key(slug);
    templates
        .iter()
        .find(|model| {
            !model_is_hidden(model)
                && model_instruction_source(model).is_some()
                && model
                    .get("slug")
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| model_family_key(candidate) == family)
        })
        .or_else(|| {
            templates
                .iter()
                .find(|model| !model_is_hidden(model) && model_instruction_source(model).is_some())
        })
}

fn account_model_with_runtime_template(base: &Value, account: &Value) -> Value {
    let base_slug = base.get("slug").and_then(Value::as_str).unwrap_or("");
    let account_slug = account.get("slug").and_then(Value::as_str).unwrap_or("");
    let mut merged = overlay_account_fields(base, account);
    if !account_slug.is_empty() && !model_id::equal(base_slug, account_slug) {
        if account
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_none_or(|description| description.is_empty())
        {
            let name = account
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(account_slug);
            merged["description"] = json!(name);
        }
        if let Some(object) = merged.as_object_mut() {
            object.remove("upgrade");
            object.remove("availability_nux");
        }
    }
    merged
}

fn overlay_account_fields(base: &Value, account: &Value) -> Value {
    let Some(account_object) = account.as_object() else {
        return base.clone();
    };
    let mut merged = base.clone();
    let Some(merged_object) = merged.as_object_mut() else {
        return account.clone();
    };
    for (key, value) in account_object {
        if value.is_null() {
            continue;
        }
        if key == "base_instructions"
            && value
                .as_str()
                .is_none_or(|instructions| instructions.trim().is_empty())
        {
            continue;
        }
        if key == "model_messages"
            && model_instruction_source(&json!({ "model_messages": value })).is_none()
        {
            continue;
        }
        if key == "description"
            && value
                .as_str()
                .is_some_and(|description| description.trim().is_empty())
        {
            continue;
        }
        merged_object.insert(key.clone(), value.clone());
    }
    merged
}

/// Unit tests must never invoke the Codex CLI installed on the machine running
/// them: the render is slow, needs account state, and would write a real
/// snapshot. The surrounding logic is covered through the pure helpers below.
#[cfg(test)]
fn sync_runtime_catalog_snapshot(_home: &Path, _codex_app_path: &str) -> Result<bool> {
    Ok(false)
}

#[cfg(not(test))]
fn debug_model_entries(home: &Path, candidates: &[PathBuf]) -> Option<Vec<Value>> {
    for cli in candidates {
        for bundled in [false, true] {
            let Some(output) = run_codex_debug_models(cli, home, bundled) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&output) else {
                continue;
            };
            let models = official_models_from_value(&value);
            if snapshot_has_runtime_models(&models) {
                return Some(models);
            }
        }
    }
    None
}

/// A runtime snapshot is useful even when upstream has added or retired a
/// model that Codey has not learned about yet. The account-backed directory
/// only needs at least one complete model entry; the fixed list remains the
/// fallback for installations that cannot produce a snapshot at all.
fn snapshot_has_runtime_models(models: &[Value]) -> bool {
    models
        .iter()
        .any(|model| model_instruction_source(model).is_some())
}

#[cfg(test)]
fn snapshot_covers_official_models(models: &[Value]) -> bool {
    OFFICIAL_MODELS.iter().all(|(slug, _)| {
        models.iter().any(|model| {
            model.get("slug").and_then(Value::as_str) == Some(*slug)
                && model_instruction_source(model).is_some()
        })
    })
}

/// Candidates are ordered the way the launch resolves its own runtime: the
/// configured app directory first, then the staged copy Codey keeps for
/// packages that cannot be executed in place, then any other installed copy.
#[cfg(not(test))]
fn codex_cli_candidates_for(codex_app_path: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push = |path: PathBuf| {
        if path.is_file() && !candidates.contains(&path) {
            candidates.push(path);
        }
    };
    let configured = codex_app_path.trim();
    if !configured.is_empty()
        && let Some(app_dir) =
            codey_runtime_core::app_paths::resolve_codex_app_dir(Some(Path::new(configured)))
        && let Some(executable) = codey_runtime_core::app_paths::codex_runtime_executable(&app_dir)
    {
        push(executable);
    }
    // The staged copy is verified runnable (the WindowsApps package location
    // needs special execution rights), so it comes before the app resources.
    for path in hashed_cli_dirs("Codey", "codex-runtime") {
        push(path.join("codex.exe"));
    }
    if let Some(app_dir) = codey_runtime_core::app_paths::resolve_codex_app_dir(None)
        && let Some(executable) = codey_runtime_core::app_paths::codex_runtime_executable(&app_dir)
    {
        push(executable);
    }
    for path in hashed_cli_dirs("OpenAI", "Codex") {
        push(path.join("codex.exe"));
    }
    if let Some(path) = cli_on_path() {
        push(path);
    }
    candidates
}

/// `%LOCALAPPDATA%\<vendor>\<dir>\<hash>` hosts the standalone Codex CLI and
/// Codey's staged copies; newest first so an updated install wins.
#[cfg(all(windows, not(test)))]
fn hashed_cli_dirs(vendor: &str, dir: &str) -> Vec<PathBuf> {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let mut root = PathBuf::from(local_app_data).join(vendor).join(dir);
    // The standalone installer nests the hash directories under `bin`.
    if vendor == "OpenAI" {
        root = root.join("bin");
    }
    let mut entries = std::fs::read_dir(&root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    entries.sort_by_key(|path| {
        std::cmp::Reverse(
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok(),
        )
    });
    entries
}

#[cfg(all(not(windows), not(test)))]
fn hashed_cli_dirs(_vendor: &str, _dir: &str) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(not(test))]
fn cli_on_path() -> Option<PathBuf> {
    let name = if cfg!(windows) { "codex.exe" } else { "codex" };
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(not(test))]
const DEBUG_MODELS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(not(test))]
fn run_codex_debug_models(cli: &Path, home: &Path, bundled: bool) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut command = Command::new(cli);
    command.args(["debug", "models"]);
    if bundled {
        command.arg("--bundled");
    }
    command
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(codey_runtime_core::windows_create_no_window());
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // The catalog is larger than the pipe buffer, so it must be drained while
    // the deadline is being polled.
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        stdout.read_to_end(&mut buffer).ok().map(|_| buffer)
    });
    let deadline = std::time::Instant::now() + DEBUG_MODELS_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = reader.join().ok().flatten();
                return status.success().then_some(()).and(output);
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
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
    let slug = model.get("slug").and_then(Value::as_str).unwrap_or("");
    with_forwarded_ultra(slug, efforts)
}

/// Luna 的上游目录停在 max，但桌面档位选择器仍提供 ultra。不补上这一档时，
/// 客户端会把选中的 ultra 退回默认强度。
fn forwards_ultra_despite_template(slug: &str) -> bool {
    let slug = route_scoped_upstream_model_id(slug);
    model_id::equal(slug, "gpt-6-luna") || model_id::equal(slug, "gpt-5.6-luna")
}

fn with_forwarded_ultra(slug: &str, mut efforts: Vec<String>) -> Vec<String> {
    if forwards_ultra_despite_template(slug)
        && efforts.iter().any(|effort| effort == "max")
        && !efforts.iter().any(|effort| effort == "ultra")
    {
        efforts.push("ultra".to_string());
    }
    efforts
}

fn ensure_forwarded_ultra_level(model: &mut Value) {
    let slug = model.get("slug").and_then(Value::as_str).unwrap_or("");
    let current = reasoning_efforts_from_value(model);
    if with_forwarded_ultra(slug, current.clone())
        .iter()
        .any(|effort| effort == "ultra")
        && !current.iter().any(|effort| effort == "ultra")
        && let Some(levels) = model
            .get_mut("supported_reasoning_levels")
            .and_then(Value::as_array_mut)
    {
        levels.push(json!({
            "effort": "ultra",
            "description": reasoning_level_description("ultra"),
        }));
    }
}

fn third_party_template_supports_ultra(model: &Value) -> bool {
    let is_supported_gpt = model
        .get("slug")
        .and_then(Value::as_str)
        .is_some_and(|slug| {
            [
                "gpt-6-astra",
                "gpt-6-sol",
                "gpt-6-luna",
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
    crate::model_id::parse_alias(model_id)
        .map(|alias| alias.upstream_model)
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
    declarations: Option<
        &std::collections::BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>,
    >,
) -> Vec<ThirdPartyModelAvailability> {
    let availability = |slug: String, entry: Option<&Value>| {
        let auto_supported_reasoning_efforts = entry
            .map(third_party_reasoning_efforts_from_value)
            .unwrap_or_else(fallback_third_party_reasoning_efforts);
        let reasoning_efforts = declarations
            .into_iter()
            .flat_map(|declarations| declarations.iter())
            .find(|(model, _)| model_id::equal(model, &slug))
            .map(|(_, efforts)| efforts.clone());
        let supported_reasoning_efforts = match &reasoning_efforts {
            Some(efforts) => model_id::dedupe_preserving_first(
                efforts.iter().map(|effort| effort.value.as_str()),
            ),
            None => auto_supported_reasoning_efforts.clone(),
        };
        let default_reasoning_effort = supported_reasoning_efforts
            .iter()
            .find(|effort| effort.as_str() == THIRD_PARTY_DEFAULT_REASONING_EFFORT)
            .cloned()
            .or_else(|| supported_reasoning_efforts.first().cloned())
            .unwrap_or_else(|| THIRD_PARTY_DEFAULT_REASONING_EFFORT.to_string());
        ThirdPartyModelAvailability {
            slug,
            supported_reasoning_efforts,
            auto_supported_reasoning_efforts,
            reasoning_efforts: reasoning_efforts.unwrap_or_default(),
            default_reasoning_effort,
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

fn model_is_runtime_source_compatible(model: &Value) -> bool {
    model_instruction_source(model).is_some() && model_has_runtime_description(model)
}

fn source_models_are_runtime_compatible(models: &[Value]) -> bool {
    !models.is_empty() && models.iter().all(model_is_runtime_source_compatible)
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

/// Whether one runtime catalog id is a route alias of a derived official
/// account route rather than a model the user declared on a third-party route.
fn is_official_route_alias(model_id: &str) -> bool {
    crate::model_id::parse_alias(model_id)
        .is_some_and(|alias| crate::config::is_official_profile_id(alias.provider_key))
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
    if !preserve_source_runtime_metadata
        || model
            .get("context_window")
            .and_then(Value::as_u64)
            .is_none_or(|window| window == 0)
    {
        // ponytail: unknown providers use a 200K operating budget; an
        // explicit provider/model setting replaces it when capacity is known.
        model["context_window"] = json!(200_000);
        model["max_context_window"] = json!(200_000);
        model["effective_context_window_percent"] = json!(95);
        model["auto_compact_token_limit"] = Value::Null;
        model["codey_context_source"] = json!("conservative_fallback");
        if let Some(object) = model.as_object_mut() {
            object.remove("codey_context_base");
        }
    } else {
        model["codey_context_source"] = json!("official_catalog");
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

/// Chat Completions 与 Anthropic Messages 都没有 `input_image.detail=original`
/// 的对应字段，Codex 只会按运行时目录里声明的能力决定是否发出这个值。适配
/// 线路上必须清掉该声明，否则历史里已经存在的原图请求会被本地路由拒绝。
fn gate_cached_image_detail_original(model: &mut Value, allowed_model_keys: &HashSet<String>) {
    let allowed = model
        .get("slug")
        .and_then(Value::as_str)
        .is_some_and(|slug| allowed_model_keys.contains(&model_id::key(slug)));
    if allowed {
        return;
    }
    if let Some(object) = model.as_object_mut() {
        object.remove("supports_image_detail_original");
    }
}

/// Restores the context fields of a reused catalog to their declared values and
/// migrates the larger window an older Codey version wrote for selected models.
fn prepare_cached_context_window(model: &mut Value) {
    if model.get("codey_source").and_then(Value::as_str) == Some("third_party")
        && model.get("codey_context_source").is_none()
        && !model
            .get("slug")
            .and_then(Value::as_str)
            .is_some_and(|slug| {
                let upstream = slug.split_once('/').map_or(slug, |(_, model)| model);
                default_official_model_slugs()
                    .iter()
                    .any(|official| model_id::equal(official, upstream))
            })
    {
        model["context_window"] = json!(200_000);
        model["max_context_window"] = json!(200_000);
        model["effective_context_window_percent"] = json!(95);
        model["auto_compact_token_limit"] = Value::Null;
        model["codey_context_source"] = json!("conservative_fallback");
    }
    // Preserve the unmodified declaration so cached catalogs can remove an
    // override without inheriting stale window/threshold values.
    const FIELDS: [&str; 5] = [
        "context_window",
        "max_context_window",
        "effective_context_window_percent",
        "auto_compact_token_limit",
        "codey_context_source",
    ];
    if model.get("codey_context_base").is_none()
        && matches!(
            model.get("codey_context_source").and_then(Value::as_str),
            None | Some("legacy_1m")
        )
        && model.get("context_window").and_then(Value::as_u64) == Some(LEGACY_1M_CONTEXT_WINDOW)
    {
        // Migrate a pre-baseline Codey override once. A source explicitly
        // identified as official metadata must retain its declared window.
        model["context_window"] = json!(DEFAULT_CONTEXT_WINDOW);
        model["max_context_window"] = json!(DEFAULT_CONTEXT_WINDOW);
        model["effective_context_window_percent"] = json!(DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT);
        model["auto_compact_token_limit"] = Value::Null;
    }
    if let Some(base) = model.get("codey_context_base").cloned() {
        for field in FIELDS {
            if let Some(value) = base.get(field) {
                model[field] = value.clone();
            } else if let Some(object) = model.as_object_mut() {
                object.remove(field);
            }
        }
    } else {
        let mut base = serde_json::Map::new();
        for field in FIELDS {
            if let Some(value) = model.get(field) {
                base.insert(field.to_string(), value.clone());
            }
        }
        model["codey_context_base"] = Value::Object(base);
    }
}

#[cfg(test)]
#[test]
fn cached_context_projection_preserves_missing_fields_and_is_idempotent() {
    for mut model in [
        json!({"slug": "route/gpt-5.6-sol", "codey_source": "third_party"}),
        json!({"slug": "gpt-5.6-sol", "context_window": 272000, "auto_compact_token_limit": null}),
    ] {
        let original = model.clone();
        prepare_cached_context_window(&mut model);
        let projected = model.clone();
        prepare_cached_context_window(&mut model);
        assert_eq!(model, projected);
        model.as_object_mut().unwrap().remove("codey_context_base");
        assert_eq!(model, original);
    }
}

#[cfg(test)]
#[test]
fn model_context_projection_restores_cache_and_explicit_overrides_legacy() {
    use crate::config::ModelContextConfig;
    let mut model = json!({ "slug": "route/custom", "codey_source": "third_party", "context_window": 272000, "max_context_window": 872000 });
    prepare_cached_context_window(&mut model);
    assert_eq!(model["context_window"], 200_000);
    assert_eq!(model["codey_context_source"], "conservative_fallback");
    let policy = ModelContextConfig {
        context_window_tokens: 100_000,
        auto_compact_token_limit: Some(80_000),
        reserve_output_tokens: Some(12_345),
    };
    apply_model_context(&mut model, Some(&policy)).unwrap();
    assert_eq!(model["context_window"], 100_000);
    assert_eq!(model["max_context_window"], 100_000);
    assert_eq!(model["effective_context_window_percent"], 87);
    assert_eq!(model["auto_compact_token_limit"], 80_000);
    assert_eq!(model["codey_context_source"], "user_declared");
    prepare_cached_context_window(&mut model);
    assert_eq!(model["context_window"], 200_000);
    assert_eq!(model["max_context_window"], 200_000);
    assert_eq!(model["effective_context_window_percent"], 95);
    assert!(model["auto_compact_token_limit"].is_null());
    let mut trusted = json!({ "slug": "gpt-5.5", "context_window": 272000, "max_context_window": 872000, "effective_context_window_percent": 95 });
    prepare_cached_context_window(&mut trusted);
    assert_eq!(trusted["max_context_window"], 872000);
    let mut native_large = json!({"slug":"official", "context_window":1000000, "max_context_window":1000000, "effective_context_window_percent":95, "codey_context_source":"official_catalog"});
    prepare_cached_context_window(&mut native_large);
    assert_eq!(native_large["context_window"], 1000000);
    assert_eq!(native_large["effective_context_window_percent"], 95);
    let mut legacy_override = json!({ "slug": "route/legacy", "codey_source": "third_party", "context_window": 1000000, "max_context_window": 1000000, "effective_context_window_percent": 100, "auto_compact_token_limit": null, "codey_context_source": "legacy_1m" });
    prepare_cached_context_window(&mut legacy_override);
    assert_eq!(legacy_override["context_window"], 272_000);
    assert_eq!(legacy_override["max_context_window"], 272_000);
    assert_eq!(legacy_override["effective_context_window_percent"], 95);
}

/// Writes the catalog and returns the exact bytes that now live on disk.
fn write_catalog(home: &Path, models: &[Value]) -> Result<Vec<u8>> {
    let mut catalog = serde_json::to_vec_pretty(&json!({ "models": models }))
        .context("序列化 Codey 模型目录失败")?;
    catalog.push(b'\n');
    let path = home.join(relative_path());
    if fs::read(&path).is_ok_and(|current| current == catalog) {
        protect_catalog_file(&path)?;
        return Ok(catalog);
    }
    atomic_write(&path, &catalog)?;
    Ok(catalog)
}

fn write_verified_catalog(home: &Path, models: &[Value]) -> Result<usize> {
    let written = write_catalog(home, models)?;
    // Verify the bytes on disk instead of re-parsing the whole catalog; the
    // serialized form already carries every slug in order.
    let path = home.join(relative_path());
    let on_disk = fs::read(&path)
        .with_context(|| format!("读取 Codey 运行时模型目录失败：{}", path.display()))?;
    if on_disk != written {
        bail!("写入后的 Codey 模型目录与本次生成结果不一致");
    }
    Ok(models.len())
}

fn read_catalog_value(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

fn read_runtime_catalog_models(home: &Path) -> Result<Vec<Value>> {
    let path = home.join(relative_path());
    read_runtime_catalog_models_at(&path)
}

fn read_runtime_catalog_models_at(path: &Path) -> Result<Vec<Value>> {
    let bytes = fs::read(path)
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
fn level_has_runtime_description(level: &Value) -> bool {
    level
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|description| !description.is_empty())
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
                    "additional_speed_tiers": ["fast"],
                    "upgrade": {"model": "gpt-5.6-sol"}
                },
                // An older cache can still carry a model the official list has
                // retired; the generated catalog must not expose it again.
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
                    "additional_speed_tiers": ["fast"]
                },
                // Upstream retired this slug and the fixed official list no
                // longer carries it; an old cache that still holds it must not
                // bring it back into the generated catalog.
                {
                    "slug": "gpt-5.3-codex-spark",
                    "display_name": "GPT-5.3-Codex-Spark",
                    "visibility": "list",
                    "priority": 30,
                    "supported_in_api": true,
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
                "gpt-5.3-codex-spark" => {
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

    #[test]
    fn account_snapshot_exposes_models_unknown_to_the_fallback_list() {
        let home = tempfile::tempdir().unwrap();
        let snapshot = json!({
            "codey_account_snapshot": true,
            "models": [
                {
                    "slug": "gpt-6-sol",
                    "display_name": "GPT-6-Sol",
                    "visibility": "list",
                    "description": "GPT-6-Sol",
                    "base_instructions": "account instructions",
                    "supported_reasoning_levels": [{"effort": "low"}]
                },
                {
                    "slug": "gpt-6-luna",
                    "display_name": "GPT-6-Luna",
                    "visibility": "list",
                    "description": "GPT-6-Luna",
                    "base_instructions": "account instructions",
                    "supported_reasoning_levels": [{"effort": "low"}]
                }
            ]
        });
        fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
        fs::write(
            home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();

        let state = selection_state(home.path(), true, None, &[], None).unwrap();
        assert_eq!(state.official_model_ids, ["gpt-6-sol", "gpt-6-luna"]);
    }

    #[test]
    fn account_snapshot_borrows_a_local_template_for_slugs_the_fallback_list_omits() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let snapshot = json!({
            "codey_account_snapshot": true,
            "models": [
                {
                    "slug": "gpt-6-sol",
                    "display_name": "GPT-6-Sol",
                    "visibility": "list",
                    "description": "GPT-6-Sol"
                },
                {
                    "slug": "gpt-6-luna",
                    "display_name": "GPT-6-Luna",
                    "visibility": "list"
                },
                {"slug": "codex-auto-review", "visibility": "hide"}
            ]
        });
        fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
        fs::write(
            home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();

        let state = selection_state(home.path(), true, None, &[], None).unwrap();
        assert_eq!(state.official_model_ids, ["gpt-6-sol", "gpt-6-luna"]);
        let sol = state
            .official_models
            .iter()
            .find(|model| model.slug == "gpt-6-sol")
            .unwrap();
        assert!(
            sol.supported_reasoning_efforts
                .iter()
                .any(|effort| effort == "ultra")
        );
        assert_eq!(
            refresh_for_provider(home.path(), true, None, &[]).unwrap(),
            2
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        assert!(
            models
                .iter()
                .all(|model| model_instruction_source(model).is_some())
        );
        let luna = models
            .iter()
            .find(|model| model["slug"] == "gpt-6-luna")
            .unwrap();
        assert_eq!(luna["description"], "GPT-6-Luna");
        assert_eq!(luna["multi_agent_version"], "v1");
        let sol = models
            .iter()
            .find(|model| model["slug"] == "gpt-6-sol")
            .unwrap();
        assert_eq!(sol["multi_agent_version"], "v2");
        assert_eq!(sol["description"], "GPT-6-Sol");
    }

    #[test]
    fn merging_account_models_preserves_runtime_templates_for_new_slugs() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
        let snapshot = json!({
            "codey_account_snapshot": true,
            "models": [
                {
                    "slug": "gpt-5.6-sol",
                    "display_name": "GPT-5.6-Sol",
                    "visibility": "list",
                    "description": "Local Sol",
                    "base_instructions": "sol instructions",
                    "multi_agent_version": "v2"
                },
                {
                    "slug": "gpt-5.6-luna",
                    "display_name": "GPT-5.6-Luna",
                    "visibility": "list",
                    "description": "Local Luna",
                    "base_instructions": "luna instructions",
                    "multi_agent_version": "v1"
                }
            ]
        });
        fs::write(
            home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();

        merge_account_runtime_models(
            home.path(),
            &[
                json!({
                    "slug": "gpt-5.6-sol",
                    "display_name": "GPT-5.6-Sol",
                    "visibility": "list",
                    "description": "Updated Sol"
                }),
                json!({
                    "slug": "gpt-6-sol",
                    "display_name": "GPT-6-Sol",
                    "visibility": "list",
                    "description": "GPT-6-Sol"
                }),
                json!({
                    "slug": "gpt-6-luna",
                    "display_name": "GPT-6-Luna",
                    "visibility": "list"
                }),
            ],
        )
        .unwrap();

        let saved: Value = serde_json::from_slice(
            &fs::read(home.path().join(DEBUG_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = saved["models"].as_array().unwrap();
        let sol = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(sol["base_instructions"], "sol instructions");
        assert_eq!(sol["description"], "Updated Sol");
        let gpt6_sol = models
            .iter()
            .find(|model| model["slug"] == "gpt-6-sol")
            .unwrap();
        assert_eq!(gpt6_sol["base_instructions"], "sol instructions");
        assert_eq!(gpt6_sol["description"], "GPT-6-Sol");
        assert_eq!(gpt6_sol["multi_agent_version"], "v2");
        let gpt6_luna = models
            .iter()
            .find(|model| model["slug"] == "gpt-6-luna")
            .unwrap();
        assert_eq!(gpt6_luna["base_instructions"], "luna instructions");
        assert_eq!(gpt6_luna["description"], "GPT-6-Luna");
        assert_eq!(gpt6_luna["multi_agent_version"], "v1");
    }

    fn staged_catalog_refresh(home: &Path, selected: &[String], overrides: CatalogOverrides<'_>) {
        refresh_for_provider_with_capabilities(
            home,
            false,
            Some(selected),
            selected,
            CapabilityLists::default(),
            "",
        )
        .unwrap();
        let mut models = read_runtime_catalog_models(home).unwrap();
        for model in &mut models {
            let policy = model["slug"].as_str().and_then(|slug| {
                overrides
                    .contexts
                    .iter()
                    .find(|(key, _)| model_id::equal(key, slug))
                    .map(|(_, policy)| policy)
            });
            apply_model_context(model, policy).unwrap();
        }
        write_verified_catalog(home, &models).unwrap();
        apply_catalog_reasoning_efforts(home, overrides.reasoning_efforts).unwrap();
    }

    #[test]
    fn combined_catalog_refresh_preserves_staged_context_and_reasoning_results() {
        let staged = tempfile::tempdir().unwrap();
        let combined = tempfile::tempdir().unwrap();
        for home in [staged.path(), combined.path()] {
            write_cache(home);
        }
        let selected = vec![
            "route/gpt-5.6-sol".to_string(),
            "route/custom-model".to_string(),
        ];
        let contexts = std::collections::BTreeMap::from([(
            "ROUTE/GPT-5.6-SOL".to_string(),
            crate::config::ModelContextConfig {
                context_window_tokens: 256_000,
                auto_compact_token_limit: Some(200_000),
                reserve_output_tokens: Some(32_000),
            },
        )]);
        let efforts = std::collections::BTreeMap::from([(
            "route/gpt-5.6-sol".to_string(),
            vec![crate::config::ModelReasoningEffort {
                level: "high".into(),
                value: "high".into(),
            }],
        )]);
        let overrides = CatalogOverrides {
            contexts: &contexts,
            reasoning_efforts: &efforts,
        };
        staged_catalog_refresh(staged.path(), &selected, overrides);
        refresh_for_provider_with_contexts(
            combined.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists::default(),
            overrides,
            "",
        )
        .unwrap();
        assert_eq!(
            fs::read(staged.path().join(relative_path())).unwrap(),
            fs::read(combined.path().join(relative_path())).unwrap()
        );

        // Removing a declaration from a cached catalog must restore its template.
        let no_efforts = std::collections::BTreeMap::new();
        apply_catalog_reasoning_efforts(staged.path(), &no_efforts).unwrap();
        apply_catalog_overrides(
            combined.path(),
            CatalogOverrides {
                contexts: &contexts,
                reasoning_efforts: &no_efforts,
            },
        )
        .unwrap();
        assert_eq!(
            fs::read(staged.path().join(relative_path())).unwrap(),
            fs::read(combined.path().join(relative_path())).unwrap()
        );
    }

    #[test]
    fn invalid_override_keeps_previous_catalog_for_generation_and_cached_updates() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec!["route/gpt-5.6-sol".to_string()];
        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists::default(),
            "",
        )
        .unwrap();
        let path = home.path().join(relative_path());
        let original = fs::read(&path).unwrap();
        let contexts = std::collections::BTreeMap::from([(
            selected[0].clone(),
            crate::config::ModelContextConfig {
                context_window_tokens: 0,
                auto_compact_token_limit: None,
                reserve_output_tokens: None,
            },
        )]);
        let efforts = std::collections::BTreeMap::new();
        let overrides = CatalogOverrides {
            contexts: &contexts,
            reasoning_efforts: &efforts,
        };
        assert!(
            refresh_for_provider_with_contexts(
                home.path(),
                false,
                Some(&selected),
                &selected,
                CapabilityLists::default(),
                overrides,
                ""
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(apply_catalog_overrides(home.path(), overrides).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    #[ignore = "使用合成模型目录测量保存耗时，按需运行"]
    fn benchmark_combined_catalog_refresh() {
        let staged = tempfile::tempdir().unwrap();
        let combined = tempfile::tempdir().unwrap();
        let mut cache = official_cache();
        for model in cache["models"].as_array_mut().unwrap() {
            model["base_instructions"] =
                json!("synthetic instructions for catalog benchmark\n".repeat(1_000));
        }
        let cache = serde_json::to_vec(&cache).unwrap();
        for home in [staged.path(), combined.path()] {
            fs::write(home.join("models_cache.json"), &cache).unwrap();
        }
        let selected = (0..24)
            .map(|i| format!("route-{i}/gpt-5.6-sol"))
            .collect::<Vec<_>>();
        let contexts = selected
            .iter()
            .map(|slug| {
                (
                    slug.clone(),
                    crate::config::ModelContextConfig {
                        context_window_tokens: 256_000,
                        auto_compact_token_limit: None,
                        reserve_output_tokens: None,
                    },
                )
            })
            .collect();
        let efforts = std::collections::BTreeMap::new();
        let overrides = CatalogOverrides {
            contexts: &contexts,
            reasoning_efforts: &efforts,
        };
        let mut staged_us = Vec::new();
        let mut combined_us = Vec::new();
        for _ in 0..7 {
            let started = std::time::Instant::now();
            staged_catalog_refresh(staged.path(), &selected, overrides);
            staged_us.push(started.elapsed().as_micros());
            let started = std::time::Instant::now();
            refresh_for_provider_with_contexts(
                combined.path(),
                false,
                Some(&selected),
                &selected,
                CapabilityLists::default(),
                overrides,
                "",
            )
            .unwrap();
            combined_us.push(started.elapsed().as_micros());
        }
        let bytes = fs::read(staged.path().join(relative_path())).unwrap();
        assert_eq!(
            bytes,
            fs::read(combined.path().join(relative_path())).unwrap()
        );
        staged_us.sort_unstable();
        combined_us.sort_unstable();
        eprintln!(
            "catalogBytes={} stagedMedianUs={} combinedMedianUs={}",
            bytes.len(),
            staged_us[3],
            combined_us[3]
        );
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

    fn write_cache_with_image_detail_original(home: &Path) {
        let mut cache = official_cache();
        let sol = cache["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        sol["supports_image_detail_original"] = json!(true);
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
        // Upstream retired this slug, so a cache that still carries it must not
        // put it back into the generated catalog.
        assert!(
            !models
                .iter()
                .any(|model| model["slug"] == "gpt-5.3-codex-spark")
        );
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
            ]
        );
    }

    #[test]
    fn generated_catalog_follows_the_configured_model_order() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let catalog_slugs = || {
            let catalog: Value = serde_json::from_slice(
                &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
            )
            .unwrap();
            catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|model| model["slug"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };

        let official_selection = vec![
            "gpt-5.6-luna".to_string(),
            "gpt-5.5".to_string(),
            "gpt-6-astra".to_string(),
        ];
        assert_eq!(
            refresh_for_provider(home.path(), true, None, &official_selection).unwrap(),
            3
        );
        assert_eq!(catalog_slugs(), official_selection);

        // 本地路由目录里官方原生 ID 与第三方别名混排，也按线路配置的顺序输出。
        let mixed_selection = vec![
            "kimi/k3".to_string(),
            "gpt-5.6-sol".to_string(),
            "kimi/kimi-for-coding".to_string(),
        ];
        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&mixed_selection), &mixed_selection)
                .unwrap(),
            3
        );
        assert_eq!(catalog_slugs(), mixed_selection);
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
        assert_eq!(marker("gpt-5.3-codex-spark"), None);
        assert_eq!(marker("gpt-5.5"), None);
    }

    #[test]
    fn generated_catalog_keeps_leaf_models_without_v2_coordinator_markers() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec!["gpt-5.6-luna".into(), "provider-custom-model".into()];

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
        // A retired upstream slug is no longer part of the fixed official list,
        // so a route that still advertises it must not resurrect the entry.
        assert!(
            !models
                .iter()
                .any(|model| model["slug"] == "gpt-5.3-codex-spark")
        );
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
            .find(|model| model["slug"] == "gpt-5.6-luna")
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
        for slug in ["gpt-5.5", "gpt-5.6-luna"] {
            let model = models.iter().find(|model| model["slug"] == slug).unwrap();
            assert_eq!(model["description"], model["display_name"]);
        }
    }

    #[test]
    fn instruction_bearing_snapshot_entry_repairs_an_incomplete_cache_entry() {
        let home = tempfile::tempdir().unwrap();
        // Codex 26.908+ may keep a cache file that lacks runtime fields while a
        // `codex debug models` snapshot captured by Codey carries them. The
        // merge must prefer the instruction-bearing entry per slug.
        let mut incomplete = official_cache();
        for model in incomplete["models"].as_array_mut().unwrap() {
            let object = model.as_object_mut().unwrap();
            object.remove("base_instructions");
            object.remove("model_messages");
        }
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&incomplete).unwrap(),
        )
        .unwrap();
        fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
        fs::write(
            home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
            serde_json::to_vec(&official_cache()).unwrap(),
        )
        .unwrap();

        assert_eq!(
            refresh_for_provider(home.path(), true, None, &[]).unwrap(),
            OFFICIAL_MODELS.len()
        );
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
        assert!(
            sol["base_instructions"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
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
            ]
        );
        // The retired slug must not reappear just because an old cache carried
        // native fast metadata for it.
        assert!(
            !models
                .iter()
                .any(|model| model["slug"] == "gpt-5.3-codex-spark")
        );
    }

    #[test]
    fn third_party_catalog_keeps_supported_official_models_before_configured_models() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_template_only(home.path());
        let upstream = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.5".into(),
            "claude-sonnet".into(),
        ];
        let selected = upstream.clone();

        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&upstream), &selected,).unwrap(),
            3
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
            ["gpt-5.6-sol", "gpt-5.5", "claude-sonnet",]
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
        let gpt_55 = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.5")
            .unwrap();
        assert_eq!(gpt_55["visibility"], "list");
        assert!(gpt_55.get("upgrade").is_none());
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
                .find(|model| model["slug"] == "gpt-5.5")
                .unwrap(),
        );
    }

    #[test]
    fn mixed_catalog_drops_prompt_free_official_stubs_without_blocking_supported_models() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = official_cache();
        let stub = cache["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.5")
            .unwrap()
            .as_object_mut()
            .unwrap();
        stub.remove("base_instructions");
        stub.remove("model_messages");
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
        let selected = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.6-luna".into(),
            "gpt-6-astra".into(),
            "gpt-5.5".into(),
            "route-oc/deepseek-flash".into(),
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
        assert_eq!(
            models
                .iter()
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "gpt-5.6-sol",
                "gpt-5.6-luna",
                "gpt-6-astra",
                "route-oc/deepseek-flash",
            ]
        );
        for slug in ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-luna"] {
            let model = models.iter().find(|model| model["slug"] == slug).unwrap();
            assert_ne!(
                model.get("codey_source").and_then(Value::as_str),
                Some("third_party")
            );
            assert!(model_instruction_source(model).is_some());
            assert!(model_has_runtime_description(model));
        }
        let custom = models.last().unwrap();
        assert_eq!(custom["slug"], "route-oc/deepseek-flash");
        assert_eq!(custom["codey_source"], "third_party");
        assert!(model_instruction_source(custom).is_some());
    }

    #[test]
    fn mixed_catalog_keeps_selected_official_slugs_missing_from_upstream() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec!["route-oc/deepseek-flash".into()];
        let selected = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.6-luna".into(),
            "gpt-6-astra".into(),
            "route-oc/deepseek-flash".into(),
        ];

        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&upstream), &selected).unwrap(),
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
                "gpt-5.6-luna",
                "gpt-6-astra",
                "route-oc/deepseek-flash",
            ]
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
    fn cached_catalog_migrates_the_legacy_1m_context_window() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec!["route/gpt-5.6-sol".to_string(), "route/gpt-5.5".to_string()];

        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists::default(),
            "",
        )
        .unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for model in catalog["models"].as_array_mut().unwrap() {
            model["context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
            model["max_context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
            model["effective_context_window_percent"] = json!(100);
            model["auto_compact_token_limit"] = Value::Null;
            model["codey_context_source"] = json!("legacy_1m");
            model.as_object_mut().unwrap().remove("codey_context_base");
        }
        fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();

        assert!(
            prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[]).unwrap(),
            "the legacy window must be rewritten"
        );
        let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for model in catalog["models"].as_array().unwrap() {
            assert_eq!(model["context_window"], DEFAULT_CONTEXT_WINDOW);
            assert_eq!(model["max_context_window"], DEFAULT_CONTEXT_WINDOW);
            assert_eq!(
                model["effective_context_window_percent"],
                DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT
            );
            assert!(model["auto_compact_token_limit"].is_null());
        }
    }

    #[test]
    fn image_detail_original_is_gated_by_route_protocol() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_image_detail_original(home.path());
        let selected = vec![
            "route-native/gpt-5.6-sol".to_string(),
            "route-chat/gpt-5.6-sol".to_string(),
            "route-anthropic/gpt-5.6-sol".to_string(),
        ];
        let image_detail_original_models = vec!["route-native/gpt-5.6-sol".to_string()];

        assert_eq!(
            refresh_for_provider_with_capabilities(
                home.path(),
                false,
                Some(&selected),
                &selected,
                CapabilityLists {
                    image_detail_original_models: Some(&image_detail_original_models),
                    ..CapabilityLists::default()
                },
                "",
            )
            .unwrap(),
            selected.len()
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let models = catalog["models"].as_array().unwrap();
        let native = models
            .iter()
            .find(|model| model["slug"] == "route-native/gpt-5.6-sol")
            .unwrap();
        assert_eq!(native["supports_image_detail_original"], true);
        for slug in ["route-chat/gpt-5.6-sol", "route-anthropic/gpt-5.6-sol"] {
            let adapted = models.iter().find(|model| model["slug"] == slug).unwrap();
            assert!(
                adapted.get("supports_image_detail_original").is_none(),
                "{slug} 声明的能力必须随线路协议移除"
            );
        }
    }

    #[test]
    fn cached_catalog_fallback_removes_stale_image_detail_original_metadata() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_image_detail_original(home.path());
        let selected = vec!["route-chat/gpt-5.6-sol".to_string()];

        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists {
                image_detail_original_models: Some(&selected),
                ..CapabilityLists::default()
            },
            "",
        )
        .unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        let stale: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stale["models"][0]["supports_image_detail_original"], true);

        assert!(prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[]).unwrap());
        let sanitized: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(
            sanitized["models"][0]
                .get("supports_image_detail_original")
                .is_none()
        );
    }

    #[test]
    fn model_reasoning_effort_override_applies_and_restores_the_template() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let selected = vec!["route/gpt-5.6-sol".to_string()];
        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists::default(),
            "",
        )
        .unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        let find_model = |catalog: &Value| {
            catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|model| model["slug"] == "route/gpt-5.6-sol")
                .unwrap()
                .clone()
        };
        let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let baseline = find_model(&catalog);

        let overrides = std::collections::BTreeMap::from([(
            "route/gpt-5.6-sol".to_string(),
            vec![
                crate::config::ModelReasoningEffort {
                    level: "low".into(),
                    value: "low".into(),
                },
                crate::config::ModelReasoningEffort {
                    level: "high".into(),
                    value: "high".into(),
                },
            ],
        )]);
        apply_catalog_reasoning_efforts(home.path(), &overrides).unwrap();
        let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let declared = find_model(&catalog);
        assert_eq!(reasoning_efforts_from_value(&declared), ["low", "high"]);
        assert_eq!(declared["default_reasoning_level"], "low");
        assert_eq!(declared["supports_reasoning_summaries"], true);
        assert!(declared.get(REASONING_BASE_FIELD).is_some());

        apply_catalog_reasoning_efforts(home.path(), &std::collections::BTreeMap::new()).unwrap();
        let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let restored = find_model(&catalog);
        assert_eq!(
            restored.get("supported_reasoning_levels"),
            baseline.get("supported_reasoning_levels")
        );
        assert_eq!(
            restored.get("default_reasoning_level"),
            baseline.get("default_reasoning_level")
        );
        assert!(restored.get(REASONING_BASE_FIELD).is_none());
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
            CapabilityLists {
                native_web_search_models: Some(&native_web_search_models),
                ..CapabilityLists::default()
            },
            "",
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
    fn cached_catalog_fallback_removes_stale_capability_metadata() {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_native_web_search(home.path());
        let selected = vec!["route-search/gpt-5.6-sol".to_string()];

        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists {
                native_web_search_models: Some(&selected),
                ..CapabilityLists::default()
            },
            "",
        )
        .unwrap();
        let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
        let mut stale: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stale["models"][0]["supports_search_tool"], true);
        assert!(stale["models"][0]["web_search_tool_type"].is_string());
        stale["models"][0]["context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
        stale["models"][0]["max_context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
        stale["models"][0]["effective_context_window_percent"] = json!(100);
        stale["models"][0]["auto_compact_token_limit"] = Value::Null;
        stale["models"][0]["codey_context_source"] = json!("legacy_1m");
        stale["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("codey_context_base");
        stale["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("codey_source");
        fs::write(&path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

        assert!(
            prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[]).unwrap(),
            "a valid cached catalog should remain usable after stale capabilities are removed"
        );
        let sanitized: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(sanitized["models"][0].get("supports_search_tool").is_none());
        assert!(sanitized["models"][0].get("web_search_tool_type").is_none());
        assert_eq!(
            sanitized["models"][0]["context_window"],
            DEFAULT_CONTEXT_WINDOW
        );
        let sanitized_bytes = fs::read(&path).unwrap();

        assert!(prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[]).unwrap());
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
        // 已配置的模型排在最前，未同步时补入的官方模型保持固定顺序跟在后面。
        let model = models.first().unwrap();
        assert_eq!(model["slug"], "provider-fast-coder");
        assert_eq!(
            models[1..]
                .iter()
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            OFFICIAL_MODELS
                .iter()
                .map(|(slug, _)| *slug)
                .collect::<Vec<_>>()
        );
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
    fn luna_forwards_ultra_even_when_the_template_stops_at_max() {
        let luna = json!({
            "slug": "gpt-6-luna",
            "default_reasoning_level": "medium",
            "base_instructions": "luna instructions",
            "supported_reasoning_levels": [
                {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                {"effort": "xhigh"}, {"effort": "max"}
            ]
        });
        assert_eq!(
            third_party_reasoning_efforts_from_value(&luna),
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert!(!third_party_template_supports_coordination(&luna));

        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
        fs::write(
            home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
            serde_json::to_vec(&json!({
                "codey_account_snapshot": true,
                "models": [luna.clone()]
            }))
            .unwrap(),
        )
        .unwrap();
        let route = vec![
            "codey-official-account-67fa10d5-0cf6-4653-ad8b-34664dde569e/gpt-6-luna".to_string(),
        ];
        refresh_for_provider(home.path(), false, Some(&route), &route).unwrap();
        let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
        let written = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| {
                model["slug"]
                    .as_str()
                    .is_some_and(|slug| slug.ends_with("/gpt-6-luna"))
            })
            .unwrap();
        assert_eq!(
            reasoning_efforts_from_value(written),
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );

        let state = selection_state(home.path(), true, None, &["gpt-6-luna".into()], None).unwrap();
        let metadata = state
            .official_models
            .iter()
            .find(|model| model.slug == "gpt-6-luna")
            .unwrap();
        assert!(
            metadata
                .supported_reasoning_efforts
                .iter()
                .any(|effort| effort == "ultra")
        );
        assert_eq!(metadata.default_reasoning_effort, "medium");
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
    fn official_route_aliases_without_a_local_runtime_template_are_dropped() {
        let home = tempfile::tempdir().unwrap();
        // One official model has no runtime instructions in this machine's
        // sources, and an older config still selects a slug upstream retired.
        let mut cache = official_cache();
        let prompt_free = cache["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|model| model["slug"] == "gpt-5.5")
            .unwrap()
            .as_object_mut()
            .unwrap();
        prompt_free.remove("base_instructions");
        prompt_free.remove("model_messages");
        fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();

        let first = crate::config::official_profile_id("acct-one");
        let second = crate::config::official_profile_id("acct-two");
        let selected = vec![
            format!("{first}/gpt-5.5"),
            format!("{first}/gpt-5.4"),
            format!("{second}/gpt-5.6-sol"),
        ];

        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&selected), &selected).unwrap(),
            1
        );
        let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
        assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
        assert_eq!(catalog["models"][0]["slug"], selected[2].as_str());
        assert!(is_available(home.path()));

        // The same upstream name on a user-declared third-party route is not
        // affected by the retirement, so it keeps the generic template.
        let third_party = vec!["relay/gpt-5.4".to_string()];
        assert_eq!(
            refresh_for_provider(home.path(), false, Some(&third_party), &third_party).unwrap(),
            1
        );
        let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
        assert_eq!(catalog["models"][0]["slug"], "relay/gpt-5.4");
        assert!(model_instruction_source(&catalog["models"][0]).is_some());
        assert!(model_has_runtime_description(&catalog["models"][0]));
        assert!(is_available(home.path()));
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
        let models = catalog["models"].as_array().unwrap();
        // The stale catalog claimed native fast support for every slug; the
        // regenerated one must rebuild that metadata from the bundled catalog
        // and drop the retired slug entirely.
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
            ]
        );
        assert!(
            !models
                .iter()
                .any(|model| model["slug"] == "gpt-5.3-codex-spark")
        );
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
    fn synced_official_account_keeps_unchecked_models_disabled() {
        let home = tempfile::tempdir().unwrap();
        write_cache(home.path());
        let upstream = vec![
            "gpt-5.6-sol".into(),
            "gpt-5.6-luna".into(),
            "gpt-5.5".into(),
        ];
        let selected = vec!["gpt-5.6-sol".into()];

        let state = selection_state(
            home.path(),
            true,
            Some(&upstream),
            &selected,
            Some("gpt-5.6-luna"),
        )
        .unwrap();

        assert_eq!(state.default_model, "gpt-5.6-sol");
        assert_eq!(
            state
                .official_models
                .iter()
                .filter(|model| model.supported)
                .map(|model| model.slug.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.6-sol"]
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

    #[test]
    fn runtime_snapshot_is_stale_without_a_snapshot_or_after_a_codex_upgrade() {
        let stale = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let fresh = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(200);

        // No local Codex CLI means Codey cannot capture anything, so it must not
        // treat a missing snapshot as work it can do.
        assert!(!runtime_snapshot_is_stale_at(None, None));
        assert!(!runtime_snapshot_is_stale_at(Some(stale), None));
        // A missing snapshot is stale whenever a CLI is available: Codex
        // 26.908+ leaves no `models_cache.json`, so this is the only path that
        // ever creates the first capture.
        assert!(runtime_snapshot_is_stale_at(None, Some(fresh)));
        // A snapshot older than the newest local build is stale; one newer than
        // every candidate is not.
        assert!(runtime_snapshot_is_stale_at(Some(stale), Some(fresh)));
        assert!(!runtime_snapshot_is_stale_at(Some(fresh), Some(stale)));
        assert!(!runtime_snapshot_is_stale_at(Some(fresh), Some(fresh)));
    }

    #[test]
    fn snapshot_sync_slot_is_claimed_once_per_codex_build() {
        let attempted = std::sync::Mutex::new(None);
        let first = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10);
        let second = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(20);

        assert!(claim_snapshot_sync_slot(&attempted, Some(first)));
        // The same build is not rendered twice in one process.
        assert!(!claim_snapshot_sync_slot(&attempted, Some(first)));
        // A failed render releases the claim, so a restart can retry.
        release_snapshot_sync_slot(&attempted, Some(first));
        assert!(claim_snapshot_sync_slot(&attempted, Some(first)));
        // A Codex upgrade changes the stamp and is rendered again.
        assert!(claim_snapshot_sync_slot(&attempted, Some(second)));
        // A release that does not own the current claim leaves it in place.
        release_snapshot_sync_slot(&attempted, Some(first));
        assert!(!claim_snapshot_sync_slot(&attempted, Some(second)));
    }

    #[test]
    fn codex_cli_stamp_uses_the_newest_candidate_and_ignores_missing_paths() {
        let home = tempfile::tempdir().unwrap();
        let older = home.path().join("older-codex");
        let newer = home.path().join("newer-codex");
        fs::write(&older, b"old").unwrap();
        fs::write(&newer, b"new").unwrap();
        let older_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let newer_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(200);
        std::fs::File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(older_mtime)
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&newer)
            .unwrap()
            .set_modified(newer_mtime)
            .unwrap();

        // The newest build decides, whichever order the candidates arrive in,
        // and a path that does not exist never contributes a stamp.
        for candidates in [
            vec![older.clone(), newer.clone()],
            vec![newer.clone(), older.clone()],
        ] {
            assert_eq!(codex_cli_stamp_for(&candidates), Some(newer_mtime));
        }
        assert_eq!(codex_cli_stamp_for(&[]), None);
        assert_eq!(codex_cli_stamp_for(&[home.path().join("absent")]), None);
        assert_eq!(
            codex_cli_stamp_for(&[home.path().join("absent"), newer.clone()]),
            Some(newer_mtime)
        );
    }

    #[test]
    fn snapshot_requires_every_fixed_official_model() {
        let mut cache = official_cache();
        let models = official_models_from_value(&cache);
        assert!(snapshot_covers_official_models(&models));

        // A render missing one fixed slug, or carrying it without runtime
        // fields, must not be persisted as a durable source.
        for slug in OFFICIAL_MODELS.iter().map(|(slug, _)| *slug) {
            let mut partial = models.clone();
            partial.retain(|model| model["slug"] != slug);
            assert!(
                !snapshot_covers_official_models(&partial),
                "{slug} should be required"
            );

            let mut stripped = models.clone();
            let entry = stripped
                .iter_mut()
                .find(|model| model["slug"] == slug)
                .unwrap()
                .as_object_mut()
                .unwrap();
            entry.remove("base_instructions");
            entry.remove("model_messages");
            assert!(
                !snapshot_covers_official_models(&stripped),
                "{slug} needs runtime fields"
            );
        }

        cache["models"] = json!([]);
        assert!(!snapshot_covers_official_models(
            &official_models_from_value(&cache)
        ));
    }

    #[test]
    fn snapshot_accepts_the_real_codex_26_908_render() {
        // The 26.908 CLI renders these eleven slugs on a clean CODEX_HOME. The
        // render must be accepted, so no fixed entry may name a slug the
        // installed Codex no longer publishes: a single retired slug in
        // `OFFICIAL_MODELS` would reject every real render and silently leave
        // the missing cache unrepaired.
        let rendered = [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ]
        .into_iter()
        .chain([
            "gpt-daybreak-blue-latest",
            "gpt-daybreak-red-latest",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.2",
            "codex-auto-review",
        ])
        .map(|slug| {
            json!({
                "slug": slug,
                "display_name": slug,
                "description": format!("{slug} description"),
                "base_instructions": format!("{slug} instructions"),
            })
        })
        .collect::<Vec<_>>();

        assert!(snapshot_covers_official_models(&rendered));
        assert!(
            !rendered
                .iter()
                .any(|model| model["slug"] == "gpt-5.3-codex-spark")
        );
    }

    #[test]
    fn codex_26_908_snapshot_alone_generates_the_catalog() {
        // End-to-end for the bug this change set targets: Codex 26.908 writes
        // no `models_cache.json`, so the captured snapshot is the only
        // instruction-bearing source. A retired slug left in `OFFICIAL_MODELS`
        // would reject the render and force the custom-context recovery dialog
        // on every launch.
        let home = tempfile::tempdir().unwrap();
        let models = [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-daybreak-blue-latest",
            "gpt-daybreak-red-latest",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.2",
            "codex-auto-review",
        ]
        .into_iter()
        .map(|slug| {
            json!({
                "slug": slug,
                "display_name": slug,
                "description": format!("{slug} description"),
                "base_instructions": format!("{slug} instructions"),
                "supported_reasoning_levels": [{"effort": "low"}],
            })
        })
        .collect::<Vec<_>>();
        let snapshot = json!({ "models": models });
        fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
        fs::write(
            home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        assert!(!home.path().join("models_cache.json").exists());

        assert_eq!(
            refresh_for_provider(home.path(), true, None, &[]).unwrap(),
            OFFICIAL_MODELS.len()
        );
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|model| model["slug"].as_str().unwrap())
                .collect::<Vec<_>>(),
            OFFICIAL_MODELS
                .iter()
                .map(|(slug, _)| *slug)
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn newer_snapshot_supersedes_a_stale_cache_entry() {
        // A capture happens when the snapshot is missing or older than the
        // installed Codex, so a snapshot that outdates `models_cache.json`
        // carries the instructions the cache cannot. Reading sources in a
        // fixed order would let the stale cache win and silently waste the
        // capture, leaving upgraded installs on pre-upgrade instructions.
        let home = tempfile::tempdir().unwrap();
        let source = |label: &str| {
            json!({
                "models": OFFICIAL_MODELS
                    .iter()
                    .map(|(slug, _)| {
                        json!({
                            "slug": slug,
                            "display_name": slug,
                            "description": format!("{label} description"),
                            "base_instructions": format!("{label} instructions for {slug}"),
                        })
                    })
                    .collect::<Vec<_>>()
            })
        };
        let cache = home.path().join("models_cache.json");
        let snapshot = home.path().join(DEBUG_CATALOG_RELATIVE_PATH);
        fs::write(&cache, serde_json::to_vec(&source("CACHED")).unwrap()).unwrap();
        fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
        fs::write(&snapshot, serde_json::to_vec(&source("SNAPSHOT")).unwrap()).unwrap();

        let older = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let newer = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(200);
        for (path, stamp) in [(cache, older), (snapshot, newer)] {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(stamp)
                .unwrap();
        }

        let entries = read_official_entries(home.path()).unwrap();
        let sol = entries
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(
            sol["base_instructions"],
            "SNAPSHOT instructions for gpt-5.6-sol"
        );

        // The reverse order still prefers the cache, so an install whose Codex
        // keeps writing its own cache does not inherit Codey snapshot text.
        std::fs::File::options()
            .write(true)
            .open(home.path().join("models_cache.json"))
            .unwrap()
            .set_modified(newer + std::time::Duration::from_secs(100))
            .unwrap();
        let entries = read_official_entries(home.path()).unwrap();
        let sol = entries
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(
            sol["base_instructions"],
            "CACHED instructions for gpt-5.6-sol"
        );
    }
}
