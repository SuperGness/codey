use super::*;

pub(crate) fn native_upstream_model(config: &CodeyConfig, model: &str) -> String {
    let model = model.trim();
    if let Some(raw) = config
        .upstream_models_by_provider
        .values()
        .chain(config.selected_models_by_provider.values())
        .flatten()
        .find(|raw| model_id::equal(raw, model))
    {
        return raw.clone();
    }
    config
        .runtime_model_targets()
        .into_iter()
        .find(|target| model_id::equal(&target.alias, model))
        .map(|target| target.upstream_model)
        .or_else(|| native_provider_prefixed_model(config, model))
        .or_else(|| {
            model_id::historical_source(model, &config.model_alias_history).map(str::to_string)
        })
        .unwrap_or_else(|| model.to_string())
}

pub(crate) fn native_provider_prefixed_model(config: &CodeyConfig, model: &str) -> Option<String> {
    for profile in &config.profiles {
        let provider_id = profile.provider_id();
        let prefix = local_router::model_alias(provider_id, "");
        let Some(upstream_model) = strip_model_provider_prefix(model, &prefix) else {
            continue;
        };
        let known_model = config
            .upstream_models_by_provider
            .get(provider_id)
            .into_iter()
            .flatten()
            .chain(
                config
                    .selected_models_by_provider
                    .get(provider_id)
                    .into_iter()
                    .flatten(),
            )
            .chain(
                config
                    .declared_official_models_by_provider
                    .get(provider_id)
                    .into_iter()
                    .flatten(),
            )
            .find(|known| model_id::equal(known, upstream_model))
            .cloned()
            .or_else(|| {
                model_catalog::default_official_model_slugs()
                    .into_iter()
                    .find(|known| model_id::equal(known, upstream_model))
            });
        if known_model.is_some() {
            return known_model;
        }
    }
    None
}

pub(crate) fn strip_model_provider_prefix<'a>(model: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.trim();
    model
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .and_then(|_| model.get(prefix.len()..))
        .map(str::trim)
        .filter(|suffix| !suffix.is_empty())
}

pub(crate) fn native_model_state_for_provider(
    config: &CodeyConfig,
    provider: &codex_provider::CurrentProvider,
    home: &std::path::Path,
) -> Result<model_catalog::ModelSelectionState, String> {
    let upstream_models = config
        .upstream_models_by_provider
        .get(provider.id.as_str())
        .map(Vec::as_slice);
    let configured_models = config
        .selected_models_by_provider
        .get(provider.id.as_str())
        .map(Vec::as_slice);
    let declared_models = config
        .declared_official_models_by_provider
        .get(provider.id.as_str());
    let selected_models = if configured_models.is_some() || declared_models.is_some() {
        let mut models = configured_models.unwrap_or_default().to_vec();
        if let Some(declared_models) = declared_models {
            models = preserve_selected_third_party_models(models, declared_models);
        }
        models
    } else if provider.official {
        Vec::new()
    } else {
        upstream_models.unwrap_or_default().to_vec()
    };
    let manual_third_party_models = if provider.official {
        &[][..]
    } else {
        config
            .manual_third_party_models_by_provider
            .get(provider.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
    };
    let requested_default = native_upstream_model(config, &config.subagent_model);
    model_catalog::selection_state_with_manual_models(
        home,
        provider.official,
        upstream_models,
        &selected_models,
        manual_third_party_models,
        Some(&requested_default),
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn native_subagent_model_state(
    config: &CodeyConfig,
    home: &std::path::Path,
) -> Result<model_catalog::ModelSelectionState, String> {
    let provider = codex_provider::current_provider(home)
        .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))?;
    native_model_state_for_provider(config, &provider, home)
}

pub(crate) fn reconcile_subagent_models_for_mode(
    config: &mut CodeyConfig,
    model_state: &model_catalog::ModelSelectionState,
) {
    if !config.local_router_enabled {
        config.subagent_model = native_upstream_model(config, &config.subagent_model);
        let native_models = config
            .subagent_roles
            .iter()
            .map(|(role, selection)| {
                (
                    role.clone(),
                    native_upstream_model(config, &selection.model),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (role, model) in native_models {
            if let Some(selection) = config.subagent_roles.get_mut(&role) {
                selection.model = model;
            }
        }
    }
    subagent_policy::reconcile_with_model_state(config, Some(model_state));
}

pub(crate) async fn current_provider_status_async(
    config: &CodeyConfig,
) -> Result<codex_provider::ProviderStatus, String> {
    if config.local_router_enabled {
        return Ok(codex_provider::status_from_config(config));
    }
    let provider = current_codex_provider().await?;
    Ok(codex_provider::ProviderStatus {
        changed: false,
        provider,
    })
}

pub(crate) async fn current_model_state_async(
    config: &CodeyConfig,
) -> Result<model_catalog::ModelSelectionState, String> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || current_model_state(&config))
        .await
        .map_err(|error| format!("读取 Codey 模型目录的任务异常退出：{error}"))?
}

pub(crate) async fn model_state_for_route_async(
    config: &CodeyConfig,
    route_id: &str,
) -> Result<model_catalog::ModelSelectionState, String> {
    let mut scoped = config.clone();
    scoped.active_profile_id = route_id.to_string();
    current_model_state_async(&scoped).await
}

pub(crate) fn current_renderer_model_catalog(config: &CodeyConfig) -> Result<Value, String> {
    let model_state = current_model_state(config)?;
    Ok(renderer_model_catalog_value(config, &model_state))
}

pub(crate) async fn current_renderer_model_catalog_async(
    config: CodeyConfig,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || current_renderer_model_catalog(&config))
        .await
        .map_err(|error| format!("读取渲染进程模型目录的任务异常退出：{error}"))?
}
