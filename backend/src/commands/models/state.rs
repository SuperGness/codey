use super::*;

#[cfg(test)]
pub(crate) fn provider_route_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.local_router_enabled != current.local_router_enabled
        || provider_route_snapshots(applied) != provider_route_snapshots(current)
        || websocket_transport_requires_restart(applied, current)
        || native_web_search_capability_requires_restart(applied, current)
        || remote_compaction_transport_requires_restart(applied, current)
}

pub(crate) fn websocket_transport_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.runtime_supports_websockets() != current.runtime_supports_websockets()
        || applied.runtime_websocket_model_aliases() != current.runtime_websocket_model_aliases()
}

pub(crate) fn remote_compaction_transport_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.runtime_supports_remote_compaction() != current.runtime_supports_remote_compaction()
}

pub(crate) fn native_web_search_capability_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.runtime_native_web_search_model_aliases()
        != current.runtime_native_web_search_model_aliases()
}

pub(crate) fn runtime_supports_current_routes_for_hot_reload(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    if applied.local_router_enabled != current.local_router_enabled {
        return false;
    }
    if !current.local_router_enabled {
        return true;
    }
    let mut capability_config = current.clone();
    for profile in &mut capability_config.profiles {
        if !profile.enabled
            && applied
                .profiles
                .iter()
                .any(|previous| previous.id == profile.id && previous.enabled)
        {
            profile.enabled = true;
        }
    }
    // Model membership can be delivered to the picker and router immediately.
    // Keep startup capability differences separate from that delivery status.
    let route_capabilities = |config: &CodeyConfig| {
        config
            .profiles
            .iter()
            .filter_map(|profile| {
                let websockets = config.route_supports_websockets_this_launch(profile);
                let web_search = config.route_supports_native_web_search_this_launch(profile);
                (websockets || web_search)
                    .then(|| (profile.provider_id().to_string(), websockets, web_search))
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    if route_capabilities(applied) != route_capabilities(&capability_config)
        || applied.runtime_supports_websockets() != capability_config.runtime_supports_websockets()
        || remote_compaction_transport_requires_restart(applied, &capability_config)
    {
        return false;
    }
    let applied = official_route_snapshots(applied);
    official_route_snapshots(current)
        .into_iter()
        .all(|(provider_id, route)| applied.get(&provider_id) == Some(&route))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderRouteSnapshot {
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) upstream_protocol: String,
    pub(crate) auth_mode: String,
    pub(crate) official_account: bool,
    pub(crate) supports_remote_compaction: bool,
    pub(crate) supports_websockets: bool,
    pub(crate) supports_native_web_search: bool,
    pub(crate) model_request_headers: BTreeMap<String, String>,
}

pub(crate) fn provider_route_snapshots(
    config: &CodeyConfig,
) -> BTreeMap<String, ProviderRouteSnapshot> {
    config
        .profiles
        .iter()
        .map(|profile| {
            (
                profile.provider_id().to_string(),
                ProviderRouteSnapshot {
                    base_url: profile.normalized_base_url(),
                    api_key: profile.api_key.trim().to_string(),
                    upstream_protocol: profile.upstream_protocol.clone(),
                    auth_mode: profile.auth_mode.clone(),
                    official_account: profile.official_account,
                    supports_remote_compaction: profile.supports_remote_compaction,
                    supports_websockets: profile.supports_websockets,
                    supports_native_web_search: profile.supports_native_web_search,
                    model_request_headers: profile.model_request_headers.clone(),
                },
            )
        })
        .collect()
}

pub(crate) fn official_route_snapshots(
    config: &CodeyConfig,
) -> BTreeMap<String, ProviderRouteSnapshot> {
    provider_route_snapshots(config)
        .into_iter()
        .filter(|(_, route)| route.official_account)
        .collect()
}

pub(crate) fn renderer_model_catalog_value(
    config: &CodeyConfig,
    model_state: &model_catalog::ModelSelectionState,
) -> Value {
    if !config.local_router_enabled {
        let mut catalog = renderer_native_model_catalog_value(model_state);
        catalog["legacy_model_aliases"] = json!(config.model_alias_history);
        let provider_id = codex_provider::current_provider(codex_home())
            .map(|provider| provider.id)
            .unwrap_or_default();
        catalog["native_model_provider"] = json!(provider_id.clone());
        if config.provider_is_disabled(&provider_id) {
            catalog["status"] = json!("ok");
            catalog["clear_models"] = json!(true);
        }
        if let Some(metadata) = catalog["model_metadata"].as_array_mut() {
            for entry in metadata {
                let supported = config.model_supports_1m_context(
                    &provider_id,
                    entry["model"].as_str().unwrap_or_default(),
                );
                entry["supports_1m_context"] = json!(supported);
                entry["context_window"] = if supported {
                    json!(1_000_000)
                } else {
                    Value::Null
                };
                entry["max_context_window"] = if supported {
                    json!(1_000_000)
                } else {
                    Value::Null
                };
            }
        }
        return catalog;
    }
    let route_catalog = renderer_route_model_catalog(config, model_state);
    let models = route_catalog
        .iter()
        .map(|entry| entry.alias.clone())
        .collect::<Vec<_>>();
    let model_metadata = route_catalog
        .iter()
        .map(|entry| {
            let mut metadata = json!({
                "model": entry.alias,
                "display_name": format!("[{}] {}", entry.route_prefix, entry.model),
                "route_name": entry.route_name,
                "route_prefix": entry.route_prefix,
                "provider_id": entry.request_provider_id,
                "source_model": entry.request_model,
                "official_account": entry.official_account,
                "supported_reasoning_efforts": entry.supported_reasoning_efforts,
                "default_reasoning_effort": entry.default_reasoning_effort,
            });
            metadata["route_provider_id"] = Value::String(entry.provider_id.clone());
            metadata["upstream_model"] = Value::String(entry.model.clone());
            metadata["model_display_name"] = Value::String(entry.model.clone());
            let supported = config.model_supports_1m_context(&entry.provider_id, &entry.model);
            metadata["supports_1m_context"] = json!(supported);
            metadata["context_window"] = if supported {
                json!(1_000_000)
            } else {
                Value::Null
            };
            metadata["max_context_window"] = if supported {
                json!(1_000_000)
            } else {
                Value::Null
            };
            metadata
        })
        .collect::<Vec<_>>();
    let default_model = route_catalog
        .iter()
        .find(|entry| entry.is_default)
        .or_else(|| route_catalog.first())
        .map(|entry| entry.alias.clone())
        .unwrap_or_default();
    let default_entry = route_catalog
        .iter()
        .find(|entry| entry.alias == default_model);
    let active_provider = default_entry
        .map(|entry| entry.request_provider_id.as_str())
        .unwrap_or_default();
    let provider_name = default_entry
        .map(|entry| entry.route_name.as_str())
        .unwrap_or(active_provider);
    json!({
        "status": if models.is_empty() { "not_configured" } else { "ok" },
        "model": default_model,
        "default_model": default_model,
        "model_provider": active_provider,
        "provider_name": provider_name,
        "models": models,
        "model_metadata": model_metadata,
        "legacy_model_aliases": config.model_alias_history,
        "sources": [],
        "responses_api": {
            "status": "unknown",
            "message": ""
        }
    })
}

pub(crate) fn renderer_native_model_catalog_value(
    model_state: &model_catalog::ModelSelectionState,
) -> Value {
    let mut metadata = model_state
        .official_models
        .iter()
        .filter(|model| model.supported)
        .map(|model| {
            json!({
                "model": model.slug,
                "display_name": model.display_name,
                "supported_reasoning_efforts": model.supported_reasoning_efforts,
                "default_reasoning_effort": model.default_reasoning_effort,
            })
        })
        .collect::<Vec<_>>();
    for model in regular_route_models(model_state.third_party_models.clone()) {
        let details = model_state
            .third_party_model_metadata
            .iter()
            .find(|details| model_id::equal(&details.slug, &model));
        let mut entry = json!({ "model": model, "display_name": model });
        if let Some(details) = details {
            entry["supported_reasoning_efforts"] = json!(details.supported_reasoning_efforts);
            entry["default_reasoning_effort"] = json!(details.default_reasoning_effort);
        }
        metadata.push(entry);
    }
    let models = metadata
        .iter()
        .map(|entry| entry["model"].clone())
        .collect::<Vec<_>>();
    let default_model = models
        .iter()
        .find(|model| model.as_str() == Some(model_state.default_model.as_str()))
        .or_else(|| models.first())
        .cloned()
        .unwrap_or_else(|| json!(""));
    json!({
        "status": if models.is_empty() { "not_configured" } else { "ok" },
        "native_selection_only": true,
        "default_model": default_model,
        "models": models,
        "model_metadata": metadata,
    })
}

#[derive(Clone)]
pub(crate) struct RendererRouteModelEntry {
    pub(crate) alias: String,
    pub(crate) provider_id: String,
    pub(crate) request_provider_id: String,
    pub(crate) request_model: String,
    pub(crate) official_account: bool,
    pub(crate) route_name: String,
    pub(crate) route_prefix: String,
    pub(crate) model: String,
    pub(crate) supported_reasoning_efforts: Vec<String>,
    pub(crate) default_reasoning_effort: String,
    pub(crate) is_default: bool,
}

pub(crate) fn renderer_route_model_catalog(
    config: &CodeyConfig,
    active_model_state: &model_catalog::ModelSelectionState,
) -> Vec<RendererRouteModelEntry> {
    let mut entries = Vec::new();
    let mut aliases = HashSet::new();
    for profile in &config.profiles {
        if !profile.enabled {
            continue;
        }
        if profile.official_account && !config.official_account_available_this_launch {
            continue;
        }
        let provider_id = profile.provider_id().trim().to_string();
        if provider_id.is_empty() {
            continue;
        }
        let selected_models = if profile.official_account {
            config.enabled_official_route_models(&provider_id)
        } else {
            config.enabled_route_models(&provider_id)
        };
        let manual_models = config
            .manual_third_party_models_by_provider
            .get(&provider_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let upstream_models = config
            .upstream_models_by_provider
            .get(&provider_id)
            .map(Vec::as_slice);
        let default_model = config.default_model_for_profile(profile);
        let state = if provider_id == config.current_provider_id().unwrap_or_default() {
            active_model_state.clone()
        } else {
            model_catalog::selection_state_with_manual_models(
                codex_home(),
                profile.official_account,
                upstream_models,
                &selected_models,
                manual_models,
                default_model.as_deref(),
            )
            .unwrap_or_default()
        };
        let route_name = profile.name.trim();
        let route_name = if route_name.is_empty() {
            provider_id.as_str()
        } else {
            route_name
        };
        let route_prefix = if profile.official_account {
            OFFICIAL_ROUTE_SHORT_NAME.to_string()
        } else {
            profile.short_name.trim().to_string()
        };
        let official_models = state
            .official_models
            .iter()
            .filter(|model| model.supported)
            .map(|model| {
                (
                    model.slug.clone(),
                    model.supported_reasoning_efforts.clone(),
                    model.default_reasoning_effort.clone(),
                )
            });
        let third_party_metadata = state
            .third_party_model_metadata
            .iter()
            .map(|model| (crate::model_id::key(&model.slug), model))
            .collect::<std::collections::HashMap<_, _>>();
        let third_party_models = state.third_party_models.iter().map(|model| {
            let metadata = third_party_metadata.get(&crate::model_id::key(model));
            (
                model.clone(),
                metadata
                    .map(|metadata| metadata.supported_reasoning_efforts.clone())
                    .unwrap_or_else(|| {
                        model_catalog::THIRD_PARTY_REASONING_EFFORTS
                            .iter()
                            .map(|effort| effort.to_string())
                            .collect::<Vec<_>>()
                    }),
                metadata
                    .map(|metadata| metadata.default_reasoning_effort.clone())
                    .unwrap_or_else(|| {
                        model_catalog::THIRD_PARTY_DEFAULT_REASONING_EFFORT.to_string()
                    }),
            )
        });
        for (model, supported_reasoning_efforts, default_reasoning_effort) in
            official_models.chain(third_party_models)
        {
            let alias = if profile.official_account {
                aliases.insert(model.clone());
                model.clone()
            } else {
                route_model_alias(&provider_id, &model, &mut aliases)
            };
            // ChatGPT validates official model ids before the local router;
            // only third-party entries may use route-qualified selectors.
            let (request_provider_id, request_model) = (
                config.runtime_gateway_provider_id().to_string(),
                model.clone(),
            );
            let is_default = default_model
                .as_deref()
                .is_some_and(|default| model_id::equal(default, &model));
            entries.push(RendererRouteModelEntry {
                alias,
                provider_id: provider_id.clone(),
                request_provider_id,
                request_model,
                official_account: profile.official_account,
                route_name: route_name.to_string(),
                route_prefix: route_prefix.clone(),
                is_default,
                model,
                supported_reasoning_efforts,
                default_reasoning_effort,
            });
        }
    }
    entries
}

pub(crate) fn route_model_alias(
    provider_id: &str,
    model: &str,
    aliases: &mut HashSet<String>,
) -> String {
    let mut alias = local_router::model_alias(provider_id, model);
    if aliases.insert(alias.clone()) {
        return alias;
    }
    let mut suffix = 2;
    loop {
        alias = format!("{}#{suffix}", local_router::model_alias(provider_id, model));
        if aliases.insert(alias.clone()) {
            return alias;
        }
        suffix += 1;
    }
}
