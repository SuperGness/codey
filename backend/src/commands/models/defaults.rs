use super::super::official_accounts::{
    refresh_official_route_after_account_change, write_official_account_route_settings,
};
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
    if target_profile.official_account && !config.official_route_usable(&target_profile) {
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
    let config = save_config_to_store(state, config).await?;
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

// 入口层仍解析 reasoningEfforts / modelContexts 参数，官方线路不接受这两类
// 变更，所以不再传入本函数。
pub struct OfficialRouteModelSave {
    pub route_id: String,
    pub models: Vec<String>,
    pub enabled: Option<bool>,
    pub show_account_usage: Option<bool>,
    pub upstream_proxy: Option<String>,
    pub base_url: Option<String>,
    pub account_id: Option<String>,
    pub route_name: Option<String>,
    pub route_short_name: Option<String>,
}

pub async fn save_official_route_models(
    state: &Arc<AppState>,
    input: OfficialRouteModelSave,
) -> Result<Value, String> {
    let OfficialRouteModelSave {
        route_id,
        models: requested_models,
        enabled: requested_enabled,
        show_account_usage: requested_show_account_usage,
        upstream_proxy: requested_upstream_proxy,
        base_url: requested_base_url,
        account_id,
        route_name,
        route_short_name,
    } = input;
    let normalized_base_url = match requested_base_url.as_deref() {
        Some(base_url) => Some(crate::codex_provider::normalize_official_gateway_base_url(
            base_url,
        )?),
        None => None,
    };
    let mut timings = ModelOperationTimings::new("save_official_route_models");
    validate_requested_model_list_bounds("官方模型", &requested_models)?;
    let _config_write_guard = state.config_write_lock.lock().await;
    timings.mark("validationAndLockMs");
    let mut config = state.config.read().await.clone();
    ensure_local_route_config_writable(&config)?;
    let route_id = route_id.trim();
    let profile_index = config
        .profiles
        .iter()
        .position(|profile| profile.id == route_id)
        .ok_or_else(|| "找不到要更新模型的官方账号线路".to_string())?;
    let profile = &config.profiles[profile_index];
    if !profile.official_account || !config.official_route_usable(profile) {
        return Err("当前线路不是本次登录可用的官方账号线路".to_string());
    }
    let provider_id = profile.provider_id().to_string();
    if let Some(enabled) = requested_enabled {
        config.profiles[profile_index].enabled = enabled;
    }
    // 参数缺席表示保持现状；空字符串表示清除代理。地址合法性由配置校验把关。
    let mut mirrored_proxy: Option<String> = None;
    if let Some(upstream_proxy) = requested_upstream_proxy.as_deref() {
        let upstream_proxy = upstream_proxy.trim().to_string();
        config.profiles[profile_index].upstream_proxy = upstream_proxy.clone();
        mirrored_proxy = Some(upstream_proxy);
    }
    // 参数缺席表示保持现状；空字符串表示恢复官方默认网关。
    if let Some(base_url) = normalized_base_url.as_ref() {
        config.profiles[profile_index].base_url = base_url.clone().unwrap_or_default();
    }
    if let Some(show_usage) = requested_show_account_usage {
        config.show_account_usage_in_header = show_usage;
    }
    let official_models = config
        .upstream_models_by_provider
        .get(&provider_id)
        .filter(|models| !models.is_empty())
        .cloned()
        .unwrap_or_else(model_catalog::default_official_model_slugs);
    // 官方线路不接受上下文预算或思考强度声明变更，保留已有配置。
    let selected_models = ordered_official_selection(&official_models, &requested_models)?;
    config
        .selected_models_by_provider
        .insert(provider_id, selected_models);
    config = config.normalize();
    config.profiles[profile_index].validate()?;
    let RefreshedModelState {
        refresh: catalog_refresh,
        model_state,
        custom_contexts_restored,
    } = refreshed_model_state_with_context_recovery(
        &mut config,
        false,
        crate::native_update_ui::confirm_context_recovery,
    )
    .await?;
    timings.mark("prepareModelsMs");
    subagent_policy::reconcile_with_model_state(&mut config, Some(&model_state));
    config = config.normalize();
    config.settings_revision = config.settings_revision.saturating_add(1);
    let config = match save_config_to_store(state, config).await {
        Ok(config) => config,
        Err(error) => {
            return Err(
                rollback_model_catalog_after_config_save_async(catalog_refresh, error).await,
            );
        }
    };
    // 官方线路在每次启动准备时按账号记录重新派生，因此代理还要写回该线路自己
    // 的账号记录，否则重启后会被派生结果覆盖。失败只降级为提示，不影响已经
    // 生效的配置。
    let mut account_override_warning: Option<String> = None;
    let route_account_id = config
        .profiles
        .iter()
        .find(|profile| profile.id == route_id)
        .and_then(|profile| profile.official_account_id.clone());
    let mut saved_account_id = None;
    if let Some(account_id) = account_id
        .as_deref()
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty())
    {
        match write_official_account_route_settings(
            state,
            account_id.to_string(),
            route_name.unwrap_or_default(),
            route_short_name.unwrap_or_default(),
            requested_upstream_proxy.clone().unwrap_or_default(),
            requested_base_url.clone().unwrap_or_default(),
        )
        .await
        {
            Ok(account_id) => saved_account_id = Some(account_id),
            Err(error) => return Err(error),
        }
    } else if let Some(upstream_proxy) = mirrored_proxy
        && let Some(account_id) = route_account_id
        && let Err(error) = persist_official_account_proxy(state, account_id, upstream_proxy).await
    {
        account_override_warning = Some(format!("线路代理未能写入官方账号记录：{error}"));
    }
    *state.config.write().await = config.clone();
    drop(_config_write_guard);
    timings.mark("saveConfigMs");
    let (mut response, subagent_hot_reload) = if saved_account_id.is_some() {
        let refresh = refresh_official_route_after_account_change(state).await?;
        timings.mark("modelDeliveryMs");
        let config = state.config.read().await.clone();
        let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &config).await;
        timings.mark("subagentReloadMs");
        let mut response = refresh;
        if let Some(object) = response.as_object_mut() {
            object.insert(
                "customContextsRestored".to_string(),
                Value::Bool(custom_contexts_restored),
            );
            object.insert("accountId".to_string(), json!(saved_account_id));
        }
        (response, subagent_hot_reload)
    } else {
        let hot_reload = hot_reload_runtime_models(state, &config, &model_state).await;
        timings.mark("modelDeliveryMs");
        let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &config).await;
        timings.mark("subagentReloadMs");
        let restart_required = runtime_config_requires_restart(state, &config).await;
        (
            hot_reload.add_to_response(json!({
                "status":"ok",
                "config":redacted_config(&config),
                "modelState":model_state,
                "customContextsRestored":custom_contexts_restored,
                "restartRequired":restart_required,
            })),
            subagent_hot_reload,
        )
    };
    if let Some(warning) = account_override_warning
        && let Some(object) = response.as_object_mut()
    {
        object.insert("warning".to_string(), Value::String(warning));
    }
    Ok(add_subagent_hot_reload_to_response(
        response,
        subagent_hot_reload,
    ))
}

/// 官方线路启用的模型按请求顺序保存，线路卡片和选择器都按这个顺序显示；
/// 模型 ID 统一回写为官方目录里的拼写。
pub(crate) fn ordered_official_selection(
    official_models: &[String],
    requested_models: &[String],
) -> Result<Vec<String>, String> {
    let official_by_key = official_models
        .iter()
        .map(|model| (model_id::key(model), model.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut selected = Vec::with_capacity(requested_models.len());
    let mut seen = HashSet::new();
    for model in requested_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
    {
        let key = model_id::key(model);
        let Some(official_model) = official_by_key.get(key.as_str()) else {
            return Err(format!("模型 {model} 不在官方模型列表中"));
        };
        if seen.insert(key) {
            selected.push((*official_model).to_string());
        }
    }
    if selected.is_empty() {
        return Err("官方账号线路至少需要保留一个模型".to_string());
    }
    Ok(selected)
}

/// Keeps the official-account store in step with the proxy edited on the
/// official route card.
async fn persist_official_account_proxy(
    state: &Arc<AppState>,
    account_id: String,
    upstream_proxy: String,
) -> Result<(), String> {
    let store = state.official_accounts();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let Some(record) = store.get(&account_id)? else {
            return Ok(());
        };
        let upstream_proxy = upstream_proxy.trim();
        let upstream_proxy = (!upstream_proxy.is_empty()).then(|| upstream_proxy.to_string());
        store.update_route_settings(
            &record.id,
            record.route_name.clone(),
            record.route_short_name.clone(),
            upstream_proxy,
            record.base_url.clone(),
        )?;
        Ok(())
    })
    .await
    .map_err(|error| format!("保存官方账号线路代理任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))
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
    if runtime.applied_config.local_router_enabled != config.local_router_enabled {
        return ModelHotReloadOutcome::default();
    }
    // 待重启的线路能力差异（例如 Responses WebSocket 开关）不阻塞模型成员送达，
    // 只把运输能力固定在启动时的取值，重启后自然切换。
    let delivered =
        if runtime_supports_current_routes_for_hot_reload(&runtime.applied_config, config) {
            config.clone()
        } else {
            config_with_launch_pinned_transport(&runtime.applied_config, config)
        };
    if delivered.local_router_enabled
        && let Err(error) = runtime.sync_local_router_routes(&delivered)
    {
        return ModelHotReloadOutcome {
            error: Some(format!("{error:#}")),
            ..ModelHotReloadOutcome::default()
        };
    }
    let expected_catalog = renderer_model_catalog_value(&delivered, model_state);
    let expected_models = expected_catalog
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    let websocket_url = runtime.renderer_websocket_url().await;
    match cdp::refresh_model_whitelist(&websocket_url, &expected_catalog).await {
        Ok(refresh) => {
            runtime.mark_model_config_applied(&delivered).await;
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
    current_model_state_at(config, codex_home())
}

pub(crate) fn current_model_state_at(
    config: &CodeyConfig,
    home: &std::path::Path,
) -> Result<model_catalog::ModelSelectionState, String> {
    if !config.local_router_enabled {
        let provider = codex_provider::current_provider(home)
            .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))?;
        return native_model_state_for_provider(config, &provider, home);
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
    let official = active_profile.official_account && config.official_route_usable(active_profile);
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
        home,
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
        config.model_reasoning_efforts_by_provider.get(provider_id),
        requested_default_model.as_deref(),
    )
    .map_err(|error| error.to_string())
}
