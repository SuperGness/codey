use super::*;

/// Context window an older Codey version wrote for models flagged as 1M capable.
pub(super) const LEGACY_1M_CONTEXT_WINDOW: u64 = 1_000_000;
pub(super) const DEFAULT_CONTEXT_WINDOW: u64 = 272_000;
pub(super) const DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT: u64 = 95;
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

pub(super) const PERSONALITY_PLACEHOLDER: &str = "{{ personality }}";
/// Official account models Codey exposes. Upstream retires a model by dropping
/// it from the Codex model cache, so a retired slug has to leave this list in
/// the same change; otherwise the picker keeps offering a model the account can
/// no longer call.
pub(super) const OFFICIAL_MODELS: [(&str, &str); 5] = [
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

#[derive(Debug)]
pub(crate) struct ContextBudgetCatalogError(pub(crate) anyhow::Error);

impl fmt::Display for ContextBudgetCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for ContextBudgetCatalogError {}

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
    pub(crate) remote_compaction_models: Option<&'a [String]>,
}

/// 生成目录时一并应用的上下文与思考等级覆盖。
#[derive(Clone, Copy)]
pub(crate) struct CatalogOverrides<'a> {
    pub(crate) plugin_contexts:
        &'a std::collections::BTreeMap<String, crate::config::ModelContextConfig>,
    pub(crate) contexts: &'a std::collections::BTreeMap<String, crate::config::ModelContextConfig>,
    pub(crate) reasoning_efforts:
        &'a std::collections::BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>,
    /// Route-qualified model id to native display name. Codex shows this field
    /// in surfaces Codey cannot patch, so aliased ids must not read as raw ids.
    pub(crate) display_names: &'a std::collections::BTreeMap<String, String>,
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
    let context = CatalogContext::new(codex_app_path);
    let models = render_catalog_for_provider(
        home,
        official_provider,
        upstream_models,
        selected_models,
        capabilities,
        &context,
    )?;
    write_verified_catalog_in_context(home, &models, &context)
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
    refresh_for_provider_in_context(
        home,
        official_provider,
        upstream_models,
        selected_models,
        capabilities,
        overrides,
        &CatalogContext::new(codex_app_path),
    )
}

pub(crate) fn refresh_for_provider_in_context(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    capabilities: CapabilityLists<'_>,
    overrides: CatalogOverrides<'_>,
    context: &CatalogContext<'_>,
) -> Result<usize> {
    let mut models = render_catalog_for_provider(
        home,
        official_provider,
        upstream_models,
        selected_models,
        capabilities,
        context,
    )?;
    apply_overrides_to_models(&mut models, overrides)?;
    write_verified_catalog_in_context(home, &models, context)
}

pub(crate) fn apply_catalog_overrides(home: &Path, overrides: CatalogOverrides<'_>) -> Result<()> {
    let mut models = read_runtime_catalog_models(home)?;
    apply_overrides_to_models(&mut models, overrides)?;
    write_verified_catalog(home, &models)?;
    Ok(())
}

/// 用户目录按完整标识补齐必需模型，仅覆盖明确配置的预算。
/// 保留外部条目的元数据，每次从原目录生成，恢复默认不会继承旧副本的预算。
pub(crate) fn render_context_catalog_overlay(
    home: &Path,
    source: &Path,
    contexts: &BTreeMap<String, crate::config::ModelContextConfig>,
    required_models: &[String],
) -> Result<Option<Vec<u8>>> {
    if contexts.is_empty() && required_models.is_empty() {
        return Ok(None);
    }
    let bytes = fs::read(source)
        .with_context(|| format!("读取自定义模型目录失败：{}", source.display()))?;
    let mut catalog: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析自定义模型目录失败：{}", source.display()))?;
    let models = catalog
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("自定义模型目录缺少 models 数组：{}", source.display()))?;
    let mut indices = BTreeMap::new();
    for (index, model) in models.iter().enumerate() {
        if let Some(slug) = model.get("slug").and_then(Value::as_str) {
            indices
                .entry(model_id::key(slug))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(index));
        }
    }
    let mut targets = contexts
        .iter()
        .map(|(model, policy)| (model_id::key(model), (model.as_str(), Some(policy))))
        .collect::<BTreeMap<_, _>>();
    for model in required_models {
        targets
            .entry(model_id::key(model))
            .or_insert((model.as_str(), None));
    }
    let mut runtime_models = None;
    let mut supplemented = false;
    for (key, (model, policy)) in targets {
        let kind = if policy.is_some() {
            "预算模型"
        } else {
            "必需模型"
        };
        let target_error = |error: anyhow::Error| -> anyhow::Error {
            if policy.is_some() {
                ContextBudgetCatalogError(error).into()
            } else {
                error
            }
        };
        let missing_model = || {
            anyhow::anyhow!(
                "自定义模型目录缺少已启用的{kind}：{model}（{}）",
                source.display()
            )
        };
        let index = match indices.get(&key) {
            Some(Some(index)) => *index,
            Some(None) => {
                return Err(target_error(anyhow::anyhow!(
                    "自定义模型目录中的{kind}重复：{model}（{}）",
                    source.display()
                )));
            }
            None => {
                let runtime_path = home.join(relative_path());
                // 普通运行时读取会去重；补齐前必须保留原数组以检查歧义。
                let fallback = match &runtime_models {
                    Some(models) => models,
                    None => {
                        let models = read_context_fallback_models(&runtime_path)
                            .with_context(missing_model)
                            .map_err(target_error)?;
                        runtime_models.insert(models)
                    }
                };
                let mut matches = fallback.iter().filter(|candidate| {
                    candidate
                        .get("slug")
                        .and_then(Value::as_str)
                        .is_some_and(|slug| model_id::key(slug) == key)
                });
                let candidate = matches
                    .next()
                    .ok_or_else(|| target_error(missing_model()))?;
                if matches.next().is_some() {
                    return Err(target_error(anyhow::anyhow!(
                        "Codey 运行时模型目录中的{kind}重复：{model}（{}）",
                        runtime_path.display()
                    )));
                }
                if !runtime_compatible_models(std::slice::from_ref(candidate)) {
                    return Err(target_error(anyhow::anyhow!(
                        "Codey 运行时模型目录中的{kind}缺少 Codex 必需字段：{model}（{}）",
                        runtime_path.display()
                    )));
                }
                let mut candidate = candidate.clone();
                normalize_third_party_compaction_identity(&mut candidate);
                if policy.is_none()
                    && candidate
                        .get("codey_context_source")
                        .and_then(Value::as_str)
                        == Some("user_declared")
                    && candidate.get("codey_context_base").is_some()
                {
                    // 仅恢复已取消的用户预算，不迁移后备元数据或移除插件预算。
                    prepare_cached_context_window(&mut candidate);
                }
                let index = models.len();
                models.push(candidate);
                indices.insert(key, Some(index));
                supplemented = true;
                index
            }
        };
        apply_model_context(&mut models[index], policy).map_err(ContextBudgetCatalogError)?;
    }
    if contexts.is_empty() && !supplemented {
        return Ok(None);
    }
    let mut bytes = serde_json::to_vec_pretty(&catalog).context("序列化模型预算副本失败")?;
    bytes.push(b'\n');
    Ok(Some(bytes))
}

fn read_context_fallback_models(path: &Path) -> Result<Vec<Value>> {
    let bytes = fs::read(path)
        .with_context(|| format!("读取 Codey 运行时模型目录失败：{}", path.display()))?;
    let catalog: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 Codey 运行时模型目录失败：{}", path.display()))?;
    catalog
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Codey 运行时模型目录缺少 models 数组：{}", path.display()))
}

pub(crate) fn prepare_context_catalog_overlay(
    home: &Path,
    source: &Path,
    contexts: &BTreeMap<String, crate::config::ModelContextConfig>,
    required_models: &[String],
) -> Result<PathBuf> {
    let Some(bytes) = render_context_catalog_overlay(home, source, contexts, required_models)?
    else {
        return Ok(source.to_path_buf());
    };
    // 内容变化使用新路径，保存设置或另一个进程启动不会改写运行中的目录。
    let path = home
        .join("model-catalogs/context-overrides")
        .join(format!("{}.json", crate::fs_util::sha256_hex(&bytes)));
    if fs::read(&path).is_ok_and(|current| current == bytes) {
        protect_catalog_file(&path)?;
    } else {
        atomic_write(&path, &bytes)?;
    }
    Ok(path)
}

pub(super) fn apply_overrides_to_models(
    models: &mut [Value],
    overrides: CatalogOverrides<'_>,
) -> Result<()> {
    for model in models {
        // Always start from the catalog declaration, including when an override
        // was removed or only a plugin policy remains.
        prepare_cached_context_window(model);
        let plugin_policy = model.get("slug").and_then(Value::as_str).and_then(|slug| {
            overrides
                .plugin_contexts
                .iter()
                .find(|(key, _)| model_id::equal(key, slug))
                .map(|(_, policy)| policy)
        });
        if let Some(policy) = plugin_policy {
            apply_model_context(model, Some(policy))?;
            model["codey_context_source"] = json!("plugin_declared");
        }
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
        if let Some(display_name) = model.get("slug").and_then(Value::as_str).and_then(|slug| {
            overrides
                .display_names
                .iter()
                .find(|(key, _)| model_id::equal(key, slug))
                .map(|(_, name)| name)
        }) {
            model["display_name"] = json!(display_name);
        }
        normalize_third_party_compaction_identity(model);
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

#[cfg(test)]
#[test]
fn compaction_threshold_uses_one_output_reserve_and_validates_boundaries() {
    for (reserve, expected) in [(None, 90000), (Some(10000), 90000), (Some(25000), 75000)] {
        let mut model = json!({});
        let mut policy = crate::config::ModelContextConfig {
            context_window_tokens: 100000,
            reserve_output_tokens: reserve,
            auto_compact_token_limit: None,
        };
        apply_model_context(&mut model, Some(&policy)).unwrap();
        assert_eq!(model["auto_compact_token_limit"], expected);
        policy.auto_compact_token_limit = Some(expected);
        assert!(apply_model_context(&mut model, Some(&policy)).is_ok());
        policy.auto_compact_token_limit = Some(expected + 1);
        assert!(apply_model_context(&mut model, Some(&policy)).is_err());
    }
}

/// Shadow copy of the declaration an override replaced, so dropping the
/// override restores the template values instead of keeping stale ones.
pub(super) const REASONING_BASE_FIELD: &str = "codey_reasoning_base";
const REASONING_DECLARATION_FIELDS: [&str; 3] = [
    "supported_reasoning_levels",
    "default_reasoning_level",
    "supports_reasoning_summaries",
];

#[cfg(test)]
pub(super) fn apply_catalog_reasoning_efforts(
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
    context: &CatalogContext<'_>,
) -> Result<Vec<Value>> {
    let CapabilityLists {
        websocket_models,
        native_web_search_models,
        image_detail_original_models,
        remote_compaction_models,
    } = capabilities;
    if !official_provider
        && upstream_models.is_some_and(|models| models.is_empty())
        && selected_models.is_empty()
    {
        return Ok(Vec::new());
    }
    let mut official_models = read_official_entries_in_context(home, context)?;
    // Codex 26.908+ may never write `models_cache.json`; capture the CLI's own
    // catalog before declaring the runtime cache unusable. A snapshot older
    // than the local Codex binary is refreshed again so a
    // Codex upgrade does not keep serving stale instructions.
    let sources_unusable = official_models
        .iter()
        .all(|model| model_instruction_source(model).is_none());
    let stale_snapshot = !sources_unusable && runtime_snapshot_is_stale(home, context);
    if sources_unusable || stale_snapshot {
        let snapshot_synced =
            sync_runtime_catalog_snapshot(home, context).unwrap_or_else(|error| {
                eprintln!("读取 Codex 命令行模型清单失败：{error:#}");
                false
            });
        if snapshot_synced {
            official_models = read_official_entries_in_context(home, context)?;
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
        ensure_catalog_compatibility(model);
        expose_supported_model(model);
        add_fast_speed_controls(model);
        if supports_ultrafast_speed(model) {
            add_ultrafast_speed_controls(model);
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
        normalize_third_party_compaction_identity(model);
        prepare_cached_context_window(model);
        // Codex 只发送模型目录里声明过的思考强度。目录没有 ultra 时，界面仍能
        // 选中它，请求会被静默改成该模型的默认档（luna 是 medium）。
        ensure_forwarded_ultra_level(model);
    }
    apply_remote_compaction_models(
        &mut catalog_models,
        remote_compaction_models.unwrap_or_default(),
    );
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
        "",
    )
}

#[allow(clippy::too_many_arguments)]
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
    codex_app_path: &str,
) -> Result<ModelSelectionState> {
    selection_state_in_context(
        home,
        official_provider,
        upstream_models,
        selected_models,
        manual_third_party_models,
        reasoning_efforts,
        requested_default_model,
        &CatalogContext::new(codex_app_path),
    )
}

/// 只读取模型选择信息。CLI 快照由启动准备和明确的刷新操作生成，打开配置页不启动进程。
#[allow(clippy::too_many_arguments)]
pub(crate) fn selection_state_in_context(
    home: &Path,
    official_provider: bool,
    upstream_models: Option<&[String]>,
    selected_models: &[String],
    manual_third_party_models: &[String],
    reasoning_efforts: Option<
        &std::collections::BTreeMap<String, Vec<crate::config::ModelReasoningEffort>>,
    >,
    requested_default_model: Option<&str>,
    context: &CatalogContext<'_>,
) -> Result<ModelSelectionState> {
    // Model provenance comes from the route, not from a slug prefix. An API-key
    // provider may legitimately expose a model whose id also appears in the
    // official catalog; it must remain a route-scoped model and go through the
    // local router instead of acquiring official-account semantics.
    let official_entries = match read_official_entries_in_context(home, context) {
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

/// Repairs catalogs written by older Codey versions that copied model-cache
/// entries without Codex's now-required `description` fields on models and
/// their reasoning levels.
pub fn is_runtime_model_cache_unavailable(error: &anyhow::Error) -> bool {
    error.is::<RuntimeModelCacheUnavailable>()
}

pub(super) fn cached_instruction_templates(home: &Path) -> Vec<Value> {
    fs::read(home.join("models_cache.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map(|value| official_models_from_value(&value))
        .unwrap_or_default()
        .into_iter()
        .filter(|model| model_instruction_source(model).is_some())
        .collect()
}

pub(super) fn complete_account_models(models: &[Value], templates: &[Value]) -> Vec<Value> {
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

pub(super) fn merge_account_model(
    existing: Option<&Value>,
    incoming: &Value,
    templates: &[Value],
) -> Value {
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

pub(super) fn complete_reasoning_metadata(model: &mut Value, fallbacks: &[&Value]) {
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

pub(super) fn normalize_official_model(
    model: &mut Value,
    slug: &str,
    display_name: &str,
    priority: usize,
) {
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

pub(super) fn reasoning_efforts_from_value(model: &Value) -> Vec<String> {
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

pub(super) fn third_party_reasoning_efforts_from_value(model: &Value) -> Vec<String> {
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

pub(super) fn third_party_template_supports_coordination(model: &Value) -> bool {
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

pub(super) fn route_scoped_upstream_model_id(model_id: &str) -> &str {
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

pub(super) fn official_models_from_value(value: &Value) -> Vec<Value> {
    catalog_models_from_value(value)
        .into_iter()
        .filter(|model| model.get("codey_source").and_then(Value::as_str) != Some("third_party"))
        .collect()
}

pub(super) fn catalog_models_from_value(value: &Value) -> Vec<Value> {
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

pub(super) fn runtime_compatible_models(models: &[Value]) -> bool {
    !models.is_empty()
        && models.iter().all(|model| {
            model
                .get("base_instructions")
                .and_then(Value::as_str)
                .is_some()
                && model_has_runtime_description(model)
        })
}

pub(super) fn model_instruction_source(model: &Value) -> Option<&str> {
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

pub(super) fn model_has_runtime_description(model: &Value) -> bool {
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

/// 匹配官方模板的第三方模型不能继承其压缩身份，否则切换模型时 Codex
/// 会先用旧线路压缩。字段缺失还能兼容会话中已经记录的旧值；改成新常量仍会触发。
pub(super) fn normalize_third_party_compaction_identity(model: &mut Value) {
    let is_third_party_alias = model.get("codey_source").and_then(Value::as_str)
        == Some("third_party")
        && model
            .get("slug")
            .and_then(Value::as_str)
            .is_some_and(|slug| {
                // 官方裸模型及派生账号优先按身份保留，兼容旧目录中的错误来源标记。
                model_id::parse_alias(slug).is_some() && !is_official_route_alias(slug)
            });
    if is_third_party_alias && let Some(object) = model.as_object_mut() {
        object.remove("comp_hash");
    }
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
    if !is_official_route_alias(model_id) {
        model["description"] = json!("Third-party API model");
        model["codey_source"] = json!("third_party");
    } else if let Some(object) = model.as_object_mut() {
        // 官方账号的线路别名沿用官方元数据，不能继承旧目录的第三方标记。
        object.remove("codey_source");
    }
    model["visibility"] = json!("list");
    model["priority"] = json!(1000 + index);
    model["supported_in_api"] = json!(true);
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
    if supports_ultrafast_speed(&model) {
        add_ultrafast_speed_controls(&mut model);
    }
    model
}

/// Restores the context fields of a reused catalog to their declared values and
/// migrates the larger window an older Codey version wrote for selected models.
pub(super) fn prepare_cached_context_window(model: &mut Value) {
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
