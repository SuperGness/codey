use super::*;

pub async fn save_selected_models(
    state: &Arc<AppState>,
    requested_official_models: Vec<String>,
    requested_third_party_models: Vec<String>,
    requested_manual_third_party_models: Vec<String>,
    requested_deleted_third_party_models: Vec<String>,
    requested_supports_auto_review: Option<bool>,
    requested_route_id: Option<String>,
) -> Result<Value, String> {
    validate_requested_model_list_bounds("官方模型", &requested_official_models)?;
    validate_requested_model_list_bounds("其他模型", &requested_third_party_models)?;
    validate_requested_model_list_bounds(
        "手动添加的其他模型",
        &requested_manual_third_party_models,
    )?;
    validate_requested_model_list_bounds(
        "待删除的其他模型",
        &requested_deleted_third_party_models,
    )?;
    validate_regular_route_model_list("官方模型", &requested_official_models)?;
    validate_regular_route_model_list("其他模型", &requested_third_party_models)?;
    validate_regular_route_model_list("手动添加的其他模型", &requested_manual_third_party_models)?;
    validate_regular_route_model_list("待删除的其他模型", &requested_deleted_third_party_models)?;
    if !state.config.read().await.local_router_enabled {
        return save_native_selected_models(
            state,
            requested_official_models,
            requested_third_party_models,
            requested_manual_third_party_models,
            requested_deleted_third_party_models,
            requested_route_id,
        )
        .await;
    }
    let _config_write_guard = state.config_write_lock.lock().await;
    let mut config = state.config.read().await.clone();
    ensure_local_route_config_writable(&config)?;
    let target_route_id = requested_route_id
        .as_deref()
        .map(str::trim)
        .filter(|route_id| !route_id.is_empty())
        .unwrap_or(config.active_profile_id.as_str());
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == target_route_id)
        .cloned()
        .ok_or_else(|| "找不到要配置模型的线路".to_string())?;
    if profile.official_account {
        return Err("官方线路不支持添加第三方模型".to_string());
    }
    let provider_id = profile.provider_id().to_string();
    let upstream_models = config
        .upstream_models_by_provider
        .get(&provider_id)
        .cloned()
        .unwrap_or_default();
    let existing_manual_models = config
        .manual_third_party_models_by_provider
        .get(&provider_id)
        .cloned()
        .unwrap_or_default();
    if !requested_official_models.is_empty() {
        return Err(
            "API Key 线路不能添加官方账号模型；该线路上游返回的同名模型请作为线路模型选择"
                .to_string(),
        );
    }
    let route_official_model_ids: &[String] = &[];
    let (supported_official, selected) = validate_manual_model_selection(
        route_official_model_ids,
        &requested_official_models,
        &requested_third_party_models,
    )?;
    let deleted_third_party_model_keys = validate_deleted_third_party_models(
        route_official_model_ids,
        &requested_deleted_third_party_models,
    )?;
    let selected = selected
        .into_iter()
        .filter(|model| !deleted_third_party_model_keys.contains(&model_id::key(model)))
        .collect::<Vec<_>>();
    validate_deleted_models_are_manual(&existing_manual_models, &deleted_third_party_model_keys)?;
    let manual_third_party_models = validate_manual_third_party_model_sources(
        route_official_model_ids,
        &selected,
        &upstream_models,
        &existing_manual_models,
        &requested_manual_third_party_models,
    )?;
    let declared_official_models = supported_official.clone();
    let mut supported_models = supported_official;
    preserve_selected_third_party_models_except(
        &mut supported_models,
        &upstream_models,
        &deleted_third_party_model_keys,
    );
    preserve_selected_third_party_models_except(&mut supported_models, &selected, &HashSet::new());
    if let Some(supported) = requested_supports_auto_review {
        set_provider_auto_review_support(&mut config, &provider_id, supported);
    }
    config
        .upstream_models_by_provider
        .insert(provider_id.clone(), supported_models);
    if declared_official_models.is_empty() {
        config
            .declared_official_models_by_provider
            .remove(&provider_id);
    } else {
        config
            .declared_official_models_by_provider
            .insert(provider_id.clone(), declared_official_models);
    }
    if selected.is_empty() {
        config.selected_models_by_provider.remove(&provider_id);
        config
            .manual_third_party_models_by_provider
            .remove(&provider_id);
    } else {
        config
            .selected_models_by_provider
            .insert(provider_id.clone(), selected);
        if manual_third_party_models.is_empty() {
            config
                .manual_third_party_models_by_provider
                .remove(&provider_id);
        } else {
            config
                .manual_third_party_models_by_provider
                .insert(provider_id, manual_third_party_models);
        }
    }
    config = config.normalize();
    let (catalog_refresh, model_state) = refreshed_model_state_async(&config, false).await?;
    subagent_policy::reconcile_with_model_state(&mut config, Some(&model_state));
    config = config.normalize();
    if let Err(error) = save_config_to_store(state, &config).await {
        return Err(rollback_model_catalog_after_config_save_async(catalog_refresh, error).await);
    }
    let model_catalog_fallback = catalog_refresh
        .as_ref()
        .is_some_and(|refresh| refresh.fallback);
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
            "modelCatalogFallback":model_catalog_fallback,
            "restartRequired":restart_required,
        })),
        subagent_hot_reload,
    ))
}

pub(crate) async fn save_native_selected_models(
    state: &Arc<AppState>,
    requested_official_models: Vec<String>,
    requested_third_party_models: Vec<String>,
    requested_manual_third_party_models: Vec<String>,
    requested_deleted_third_party_models: Vec<String>,
    requested_route_id: Option<String>,
) -> Result<Value, String> {
    let previous = state.config.read().await.clone();
    if previous.local_router_enabled {
        return Err("本地路由已启用，请使用线路模型配置".to_string());
    }
    let context = native_provider_context(&previous).await?;
    if let Some(route_id) = requested_route_id
        .as_deref()
        .map(str::trim)
        .filter(|route_id| !route_id.is_empty())
        && route_id != context.route_id
        && route_id != context.provider.id
    {
        return Err("只能更新当前 Codex 线路的模型".to_string());
    }
    let current_provider = current_codex_provider().await?;
    if current_provider != context.provider {
        return Err("保存模型期间当前 Codex 线路已变化，请重试".to_string());
    }

    let _config_write_guard = state.config_write_lock.lock().await;
    let latest = state.config.read().await.clone();
    if latest.local_router_enabled {
        return Err("保存模型期间本地路由已启用，请重试".to_string());
    }
    if latest.settings_revision != previous.settings_revision {
        return Err("Codey 设置在保存模型期间已更新，请重新载入后再操作".to_string());
    }
    let mut next = config_with_native_selected_models(
        &latest,
        &context.provider,
        &requested_official_models,
        &requested_third_party_models,
        &requested_manual_third_party_models,
        &requested_deleted_third_party_models,
    )?;
    let model_state = native_model_state_for_provider(&next, &context.provider, codex_home())?;
    reconcile_subagent_models_for_mode(&mut next, &model_state);
    next = next.normalize();
    if next != latest {
        next.settings_revision = latest.settings_revision.saturating_add(1);
        save_config_to_store(state, &next)
            .await
            .map_err(|error| format!("保存当前线路模型选择失败：{error}"))?;
        *state.config.write().await = next.clone();
    }
    let public_config = redacted_config(&next);
    drop(_config_write_guard);
    let hot_reload = hot_reload_runtime_models(state, &next, &model_state).await;
    let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &next).await;
    let restart_required = runtime_config_requires_restart(state, &next).await;
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

pub(crate) fn config_with_native_selected_models(
    config: &CodeyConfig,
    provider: &codex_provider::CurrentProvider,
    requested_official_models: &[String],
    requested_third_party_models: &[String],
    requested_manual_third_party_models: &[String],
    requested_deleted_third_party_models: &[String],
) -> Result<CodeyConfig, String> {
    let provider_id = provider.id.clone();
    let mut next = config.clone();
    if provider.official {
        if !requested_third_party_models.is_empty()
            || !requested_manual_third_party_models.is_empty()
            || !requested_deleted_third_party_models.is_empty()
        {
            return Err("官方线路不支持添加第三方模型".to_string());
        }
        let official_models = model_catalog::default_official_model_slugs();
        let (selected_models, third_party_models) =
            validate_manual_model_selection(&official_models, requested_official_models, &[])?;
        if selected_models.is_empty() {
            return Err("官方账号线路至少需要保留一个模型".to_string());
        }
        if !third_party_models.is_empty() {
            return Err("官方线路不支持添加第三方模型".to_string());
        }
        next.selected_models_by_provider
            .insert(provider_id.clone(), selected_models);
        next.manual_third_party_models_by_provider
            .remove(&provider_id);
        next.declared_official_models_by_provider
            .remove(&provider_id);
        next.upstream_models_by_provider.remove(&provider_id);
        return Ok(next.normalize());
    }

    if !requested_official_models.is_empty() {
        return Err(
            "API Key 线路不能添加官方账号模型；该线路上游返回的同名模型请作为线路模型选择"
                .to_string(),
        );
    }
    let upstream_models = next
        .upstream_models_by_provider
        .get(&provider_id)
        .cloned()
        .unwrap_or_default();
    let existing_manual_models = next
        .manual_third_party_models_by_provider
        .get(&provider_id)
        .cloned()
        .unwrap_or_default();
    let route_official_model_ids: &[String] = &[];
    let (supported_official, selected) = validate_manual_model_selection(
        route_official_model_ids,
        requested_official_models,
        requested_third_party_models,
    )?;
    let deleted_third_party_model_keys = validate_deleted_third_party_models(
        route_official_model_ids,
        requested_deleted_third_party_models,
    )?;
    let selected = selected
        .into_iter()
        .filter(|model| !deleted_third_party_model_keys.contains(&model_id::key(model)))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("当前线路至少需要保留一个模型".to_string());
    }
    validate_deleted_models_are_manual(&existing_manual_models, &deleted_third_party_model_keys)?;
    let manual_third_party_models = validate_manual_third_party_model_sources(
        route_official_model_ids,
        &selected,
        &upstream_models,
        &existing_manual_models,
        requested_manual_third_party_models,
    )?;
    let mut supported_models = supported_official;
    preserve_selected_third_party_models_except(
        &mut supported_models,
        &upstream_models,
        &deleted_third_party_model_keys,
    );
    preserve_selected_third_party_models_except(&mut supported_models, &selected, &HashSet::new());
    next.upstream_models_by_provider
        .insert(provider_id.clone(), supported_models);
    next.declared_official_models_by_provider
        .remove(&provider_id);
    next.selected_models_by_provider
        .insert(provider_id.clone(), selected);
    if manual_third_party_models.is_empty() {
        next.manual_third_party_models_by_provider
            .remove(&provider_id);
    } else {
        next.manual_third_party_models_by_provider
            .insert(provider_id, manual_third_party_models);
    }
    Ok(next.normalize())
}

pub(crate) fn validate_requested_model_list_bounds(
    label: &str,
    models: &[String],
) -> Result<(), String> {
    if models.len() > provider_models::MAX_PROVIDER_MODELS {
        return Err(format!(
            "{label}数量超过安全上限 {}",
            provider_models::MAX_PROVIDER_MODELS
        ));
    }
    if models
        .iter()
        .map(|model| model.trim())
        .any(|model| model.len() > provider_models::MAX_PROVIDER_MODEL_ID_BYTES)
    {
        return Err(format!(
            "{label} ID 超过安全上限 {} 字节",
            provider_models::MAX_PROVIDER_MODEL_ID_BYTES
        ));
    }
    Ok(())
}

pub(crate) fn validate_regular_route_model_list(
    label: &str,
    models: &[String],
) -> Result<(), String> {
    if models
        .iter()
        .any(|model| model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL))
    {
        return Err(format!(
            "{label}不能包含 {}；请使用 Auto Review 线路能力开关",
            local_router::CODEX_AUTO_REVIEW_MODEL
        ));
    }
    Ok(())
}

pub(crate) fn validate_manual_model_selection(
    official_model_ids: &[String],
    requested_official_models: &[String],
    requested_third_party_models: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let official_by_key = official_model_ids
        .iter()
        .map(|model| (model_id::key(model), model.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let requested_official = requested_official_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
        .map(model_id::key)
        .collect::<HashSet<_>>();
    if let Some(model) = requested_official
        .iter()
        .find(|model| !official_by_key.contains_key(model.as_str()))
    {
        return Err(format!("模型 {model} 不在官方模型列表中"));
    }
    let supported_official = official_model_ids
        .iter()
        .filter(|model| requested_official.contains(&model_id::key(model)))
        .cloned()
        .collect::<Vec<_>>();
    let mut selected_third_party = Vec::with_capacity(requested_third_party_models.len());
    let mut seen_third_party = HashSet::<String>::with_capacity(requested_third_party_models.len());
    for model in requested_third_party_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
    {
        let key = model_id::key(model);
        if official_by_key.contains_key(&key) {
            return Err(format!(
                "模型 {model} 已在官方模型列表中，请直接勾选，不可作为其他模型手动添加"
            ));
        }
        if seen_third_party.insert(key) {
            selected_third_party.push(model.to_string());
        }
    }
    Ok((supported_official, selected_third_party))
}

pub(crate) fn validate_deleted_third_party_models(
    official_model_ids: &[String],
    requested_deleted_third_party_models: &[String],
) -> Result<HashSet<String>, String> {
    let official_model_keys = official_model_ids
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    requested_deleted_third_party_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
        .try_fold(HashSet::<String>::new(), |mut models, model| {
            let key = model_id::key(model);
            if official_model_keys.contains(key.as_str()) {
                return Err(format!("官方模型 {model} 不能作为其他模型删除"));
            }
            models.insert(key);
            Ok(models)
        })
}

pub(crate) fn validate_deleted_models_are_manual(
    manual_third_party_models: &[String],
    deleted_model_keys: &HashSet<String>,
) -> Result<(), String> {
    let manual_model_keys = manual_third_party_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    if let Some(model) = deleted_model_keys
        .iter()
        .find(|model| !manual_model_keys.contains(model.as_str()))
    {
        return Err(format!("模型 {model} 不是手动添加的其他模型，不能删除"));
    }
    Ok(())
}

pub(crate) fn validate_manual_third_party_model_sources(
    official_model_ids: &[String],
    selected_third_party_models: &[String],
    upstream_models: &[String],
    existing_manual_third_party_models: &[String],
    requested_manual_third_party_models: &[String],
) -> Result<Vec<String>, String> {
    let official_model_keys = official_model_ids
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let selected_model_keys = selected_third_party_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let upstream_model_keys = upstream_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let existing_manual_model_keys = existing_manual_third_party_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();

    let mut models = Vec::with_capacity(requested_manual_third_party_models.len());
    let mut seen = HashSet::<String>::with_capacity(requested_manual_third_party_models.len());
    for model in requested_manual_third_party_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
    {
        let key = model_id::key(model);
        if official_model_keys.contains(key.as_str()) {
            return Err(format!("官方模型 {model} 不能作为手动添加的其他模型"));
        }
        if !selected_model_keys.contains(key.as_str()) {
            continue;
        }
        if upstream_model_keys.contains(key.as_str())
            && !existing_manual_model_keys.contains(key.as_str())
        {
            continue;
        }
        if seen.insert(key) {
            models.push(model.to_string());
        }
    }
    Ok(models)
}
