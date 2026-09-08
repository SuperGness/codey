use super::*;

pub async fn delete_route(
    state: &Arc<AppState>,
    route_id: String,
    expected_revision: u64,
) -> Result<Value, String> {
    let _config_write_guard = state.config_write_lock.lock().await;
    let previous = state.config.read().await.clone();
    ensure_local_route_config_writable(&previous)?;
    ensure_route_revision(&previous, expected_revision)?;
    let route_id = route_id.trim();
    let config = config_after_route_deletion(&previous, route_id)?;
    save_config_to_store(state, &config).await?;
    *state.config.write().await = config.clone();
    let model_state = current_model_state_async(&config).await?;
    drop(_config_write_guard);
    let hot_reload = hot_reload_runtime_models(state, &config, &model_state).await;
    let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &config).await;
    let restart_required = runtime_config_requires_restart(state, &config).await;
    Ok(add_subagent_hot_reload_to_response(
        hot_reload.add_to_response(json!({
            "status":"ok",
            "config": redacted_config(&config),
            "providerStatus": codex_provider::status_from_config(&config),
            "modelState": model_state,
            "restartRequired": restart_required,
        })),
        subagent_hot_reload,
    ))
}

pub(crate) fn config_after_route_deletion(
    previous: &CodeyConfig,
    route_id: &str,
) -> Result<CodeyConfig, String> {
    if route_id == DERIVED_OFFICIAL_PROFILE_ID {
        return Err("官方账号线路由当前 Codex 登录状态管理，不能手动删除".to_string());
    }
    if previous.profiles.len() <= 1 {
        return Err("至少需要保留一条线路".to_string());
    }
    let removed_provider_id = previous
        .profiles
        .iter()
        .find(|profile| profile.id == route_id)
        .map(|profile| profile.provider_id().to_string())
        .ok_or_else(|| "找不到要删除的线路".to_string())?;
    let mut config = previous.clone();
    config
        .model_context_by_provider
        .remove(&removed_provider_id);
    config.remember_model_aliases();
    config
        .supports_1m_context_by_provider
        .remove(&removed_provider_id);
    config.profiles.retain(|profile| profile.id != route_id);
    config
        .selected_models_by_provider
        .remove(&removed_provider_id);
    config
        .manual_third_party_models_by_provider
        .remove(&removed_provider_id);
    config
        .declared_official_models_by_provider
        .remove(&removed_provider_id);
    config
        .upstream_models_by_provider
        .remove(&removed_provider_id);
    if config.active_profile_id == route_id
        && let Some(first) = config.profiles.first()
    {
        config.active_profile_id = first.id.clone();
    }
    config = config.normalize();
    config.reconcile_after_route_removal(&removed_provider_id);
    config = config.normalize();
    validate_provider_profiles(&config.profiles)?;
    config.settings_revision = previous.settings_revision.saturating_add(1);
    Ok(config)
}

pub async fn fetch_route_models(
    state: &Arc<AppState>,
    route_id: String,
    expected_revision: u64,
) -> Result<Value, String> {
    let _provider_model_sync_guard = state.provider_model_sync_lock.lock().await;
    let config = state.config.read().await.clone();
    if !config.local_router_enabled {
        return sync_native_current_provider_models(state, Some((route_id, expected_revision)))
            .await;
    }
    ensure_local_route_config_writable(&config)?;
    ensure_route_revision(&config, expected_revision)?;
    let route_id = route_id.trim();
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == route_id)
        .cloned()
        .ok_or_else(|| "找不到要同步模型的线路".to_string())?;
    if !profile.enabled {
        return Err("线路已禁用，不能同步模型".to_string());
    }
    if profile.official_account {
        return Err("官方账号线路使用官方模型目录，无需同步第三方模型".to_string());
    }
    profile.validate()?;
    let provider_id = profile.provider_id().to_string();
    let fetched_models = fetch_provider_models(profile, &state.http_client)
        .await
        .map_err(|error| error.to_string())?;
    let visible_fetched_models = regular_route_models(fetched_models.clone());
    let _config_write_guard = state.config_write_lock.lock().await;
    let mut latest = state.config.read().await.clone();
    ensure_local_route_config_writable(&latest)?;
    ensure_route_revision(&latest, expected_revision)?;
    let latest_profile = latest
        .profiles
        .iter()
        .find(|profile| profile.id == route_id)
        .ok_or_else(|| "同步模型期间线路已被删除，请重试".to_string())?;
    if latest_profile.provider_id() != provider_id {
        return Err("同步模型期间线路接入配置已变化，请重试".to_string());
    }
    latest = config_with_provider_model_sync(
        &latest,
        &provider_id,
        fetched_models.clone(),
        codex_home(),
    );
    latest.settings_revision = latest.settings_revision.saturating_add(1);
    let route_model_state = model_state_for_route_async(&latest, route_id).await?;
    let (catalog_refresh, model_state) = refreshed_model_state_async(&latest, true).await?;
    if let Err(error) = save_config_to_store(state, &latest).await {
        return Err(rollback_model_catalog_after_config_save_async(catalog_refresh, error).await);
    }
    *state.config.write().await = latest.clone();
    drop(_config_write_guard);
    let hot_reload = hot_reload_runtime_models(state, &latest, &model_state).await;
    let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &latest).await;
    let restart_required = runtime_config_requires_restart(state, &latest).await;
    Ok(add_subagent_hot_reload_to_response(
        hot_reload.add_to_response(json!({
            "status":"ok",
            "config": redacted_config(&latest),
            "providerStatus": codex_provider::status_from_config(&latest),
            "models": visible_fetched_models,
            "modelState": model_state,
            "routeModelState": route_model_state,
            "restartRequired": restart_required,
        })),
        subagent_hot_reload,
    ))
}

pub(crate) fn ensure_route_revision(
    config: &CodeyConfig,
    expected_revision: u64,
) -> Result<(), String> {
    if config.settings_revision != expected_revision {
        return Err("Codey 设置已被其他操作更新，请重新载入后再操作线路".to_string());
    }
    Ok(())
}

pub(crate) fn config_with_provider_model_sync(
    config: &CodeyConfig,
    provider_id: &str,
    provider_models: Vec<String>,
    codex_home: &std::path::Path,
) -> CodeyConfig {
    let supports_auto_review = models_support_auto_review(&provider_models);
    let provider_models = regular_route_models(provider_models);
    let selected_models = config
        .selected_models_by_provider
        .get(provider_id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let declared_models = config
        .declared_official_models_by_provider
        .get(provider_id)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let manual_models = selected_models_not_in_upstream(selected_models, &provider_models);
    let mut supported_models =
        preserve_selected_third_party_models(provider_models, selected_models);
    preserve_declared_official_models(&mut supported_models, declared_models);

    let mut next = config.clone();
    next.retain_1m_context_models(provider_id, &supported_models);
    set_provider_auto_review_support(&mut next, provider_id, supports_auto_review);
    next.upstream_models_by_provider
        .insert(provider_id.to_string(), supported_models);
    if manual_models.is_empty() {
        next.manual_third_party_models_by_provider
            .remove(provider_id);
    } else {
        next.manual_third_party_models_by_provider
            .insert(provider_id.to_string(), manual_models);
    }
    next = next.normalize();
    if next.current_provider_id() == Some(provider_id) {
        subagent_policy::reconcile_for_current_provider(&mut next, codex_home, false);
    }
    next
}
