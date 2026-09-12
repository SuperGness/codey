use super::*;

pub async fn save_default_model(
    state: &Arc<AppState>,
    requested_model: String,
    route_id: Option<String>,
) -> Result<Value, String> {
    let _config_write_guard = state.config_write_lock.lock().await;
    let mut config = state.config.read().await.clone();
    ensure_local_route_config_writable(&config)?;
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return Err("默认模型不能为空".to_string());
    }
    let target_route_id = route_id
        .as_deref()
        .map(str::trim)
        .filter(|route_id| !route_id.is_empty())
        .unwrap_or(config.active_profile_id.as_str());
    let target_profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == target_route_id)
        .cloned()
        .ok_or_else(|| "找不到要设置默认模型的线路".to_string())?;
    if target_profile.official_account && !config.official_account_available_this_launch {
        return Err("本次 Codex 没有可用的官方账号登录态，不能选择官方模型".to_string());
    }
    let target = config
        .model_target_for_route(target_route_id, requested_model)
        .ok_or_else(|| format!("模型 {requested_model} 当前不可用，无法设为默认"))?;
    config.default_model = target.alias;
    // `active_profile_id` remains a compatibility projection for older features.
    // The model default is authoritative and therefore owns that projection.
    config.active_profile_id = target_profile.id;
    config = config.normalize();
    config.settings_revision = config.settings_revision.saturating_add(1);
    let model_state = current_model_state_async(&config).await?;
    save_config_to_store(state, &config).await?;
    *state.config.write().await = config.clone();
    let public_config = redacted_config(&config);
    drop(_config_write_guard);
    let hot_reload = hot_reload_runtime_models(state, &config, &model_state).await;
    let restart_required = runtime_config_requires_restart(state, &config).await;
    Ok(hot_reload.add_to_response(json!({
        "status":"ok",
        "config":public_config,
        "modelState":model_state,
        "restartRequired":restart_required,
    })))
}

// 入口层仍解析并校验旧版 supports1MContextModels / modelContexts 参数，
// 官方线路不接受这两类变更，所以不再传入本函数。
pub async fn save_official_route_models(
    state: &Arc<AppState>,
    route_id: String,
    requested_models: Vec<String>,
    requested_enabled: Option<bool>,
    requested_show_account_usage: Option<bool>,
    requested_upstream_proxy: Option<String>,
) -> Result<Value, String> {
    validate_requested_model_list_bounds("官方模型", &requested_models)?;
    let _config_write_guard = state.config_write_lock.lock().await;
    let mut config = state.config.read().await.clone();
    ensure_local_route_config_writable(&config)?;
    let route_id = route_id.trim();
    let profile_index = config
        .profiles
        .iter()
        .position(|profile| profile.id == route_id)
        .ok_or_else(|| "找不到要更新模型的官方账号线路".to_string())?;
    let profile = &config.profiles[profile_index];
    if !profile.official_account || !config.official_account_available_this_launch {
        return Err("当前线路不是本次登录可用的官方账号线路".to_string());
    }
    let provider_id = profile.provider_id().to_string();
    if let Some(enabled) = requested_enabled {
        config.profiles[profile_index].enabled = enabled;
    }
    // 参数缺席表示保持现状；空字符串表示清除代理。地址合法性由配置校验把关。
    if let Some(upstream_proxy) = requested_upstream_proxy {
        config.profiles[profile_index].upstream_proxy = upstream_proxy.trim().to_string();
    }
    if let Some(show_usage) = requested_show_account_usage {
        config.show_account_usage_in_header = show_usage;
    }
    let official_models = model_catalog::default_official_model_slugs();
    // 官方线路不接受上下文预算或 1M 设置变更，保留已有配置。
    let official_by_key = official_models
        .iter()
        .map(|model| (model_id::key(model), model.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let requested_keys = requested_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    if requested_keys.is_empty() {
        return Err("官方账号线路至少需要保留一个模型".to_string());
    }
    if let Some(model) = requested_keys
        .iter()
        .find(|model| !official_by_key.contains_key(model.as_str()))
    {
        return Err(format!("模型 {model} 不在官方模型列表中"));
    }
    let selected_models = official_models
        .into_iter()
        .filter(|model| requested_keys.contains(&model_id::key(model)))
        .collect::<Vec<_>>();
    config
        .selected_models_by_provider
        .insert(provider_id, selected_models);
    config = config.normalize();
    let (catalog_refresh, model_state) = refreshed_model_state_async(&config, false).await?;
    subagent_policy::reconcile_with_model_state(&mut config, Some(&model_state));
    config = config.normalize();
    config.settings_revision = config.settings_revision.saturating_add(1);
    if let Err(error) = save_config_to_store(state, &config).await {
        return Err(rollback_model_catalog_after_config_save_async(catalog_refresh, error).await);
    }
    *state.config.write().await = config.clone();
    let public_config = redacted_config(&config);
    drop(_config_write_guard);
    let hot_reload = hot_reload_runtime_models(state, &config, &model_state).await;
    let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &config).await;
    let restart_required = runtime_config_requires_restart(state, &config).await;
    Ok(add_subagent_hot_reload_to_response(
        hot_reload.add_to_response(json!({
            "status":"ok",
            "config":public_config,
            "modelState":model_state,
            "restartRequired":restart_required,
        })),
        subagent_hot_reload,
    ))
}

pub(crate) async fn hot_reload_runtime_models(
    state: &Arc<AppState>,
    config: &CodeyConfig,
    model_state: &model_catalog::ModelSelectionState,
) -> ModelHotReloadOutcome {
    let runtime = state.runtime.lock().await.clone();
    let Some(runtime) = runtime else {
        return ModelHotReloadOutcome::default();
    };
    if !runtime_supports_current_routes_for_hot_reload(&runtime.applied_config, config) {
        return ModelHotReloadOutcome::default();
    }
    if config.local_router_enabled
        && let Err(error) = runtime.sync_local_router_routes(config)
    {
        return ModelHotReloadOutcome {
            error: Some(format!("{error:#}")),
            ..ModelHotReloadOutcome::default()
        };
    }
    let expected_catalog = renderer_model_catalog_value(config, model_state);
    let expected_models = expected_catalog
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    let websocket_url = runtime.renderer_websocket_url().await;
    match cdp::refresh_model_whitelist(&websocket_url, &expected_catalog).await {
        Ok(refresh) => {
            runtime.mark_model_config_applied(config).await;
            ModelHotReloadOutcome {
                reloaded: true,
                deferred: refresh.deferred,
                error: None,
            }
        }
        Err(error) => {
            let error = format!("{error:#}");
            error_log::record_failure(
                "patch_verification_failed",
                "refresh_model_whitelist",
                error.clone(),
                json!({
                    "modelCount": expected_models,
                    "websocketUrl": websocket_url,
                }),
            );
            ModelHotReloadOutcome {
                reloaded: false,
                deferred: false,
                error: Some(error),
            }
        }
    }
}

pub(crate) fn current_model_state(
    config: &CodeyConfig,
) -> Result<model_catalog::ModelSelectionState, String> {
    if !config.local_router_enabled {
        let provider = codex_provider::current_provider(codex_home())
            .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))?;
        return native_model_state_for_provider(config, &provider, codex_home());
    }
    let active_profile = config
        .profiles
        .iter()
        .find(|profile| profile.enabled && profile.id == config.active_profile_id)
        .or_else(|| config.profiles.iter().find(|profile| profile.enabled));
    let Some(active_profile) = active_profile else {
        return Ok(model_catalog::ModelSelectionState::default());
    };
    let provider_id = active_profile.provider_id();
    let official = active_profile.official_account && config.official_account_available_this_launch;
    let selected_models = if official {
        config
            .selected_models_by_provider
            .get(provider_id)
            .cloned()
            .unwrap_or_default()
    } else {
        config.enabled_route_models(provider_id)
    };
    let requested_default_model = config.default_model_for_profile(active_profile);
    model_catalog::selection_state_with_manual_models(
        codex_home(),
        official,
        config
            .upstream_models_by_provider
            .get(provider_id)
            .map(Vec::as_slice),
        &selected_models,
        config
            .manual_third_party_models_by_provider
            .get(provider_id)
            .map(Vec::as_slice)
            .unwrap_or_default(),
        requested_default_model.as_deref(),
    )
    .map_err(|error| error.to_string())
}
