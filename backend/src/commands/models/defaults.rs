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
    let hot_reload = ModelHotReloadOutcome::queued(state, false);
    drop(_config_write_guard);
    let restart_required = runtime_config_requires_restart(state, &config).await;
    Ok(hot_reload.add_to_response(json!({
        "status":"ok",
        "config":public_config,
        "modelState":model_state,
        "restartRequired":restart_required,
    })))
}

// 官方模型允许调整本地上下文预算，思考强度仍沿用官方目录。
pub struct OfficialRouteModelSave {
    pub route_id: String,
    pub models: Vec<String>,
    pub model_contexts: Option<BTreeMap<String, crate::config::ModelContextConfig>>,
    pub enabled: Option<bool>,
    pub show_account_usage: Option<bool>,
    pub upstream_proxy: Option<String>,
    pub base_url: Option<String>,
    pub account_id: Option<String>,
    pub route_name: Option<String>,
    pub route_short_name: Option<String>,
    pub max_concurrency: Option<u32>,
}

pub async fn save_official_route_models(
    state: &Arc<AppState>,
    input: OfficialRouteModelSave,
) -> Result<Value, String> {
    let OfficialRouteModelSave {
        route_id,
        models: requested_models,
        model_contexts: requested_model_contexts,
        enabled: requested_enabled,
        show_account_usage: requested_show_account_usage,
        upstream_proxy: requested_upstream_proxy,
        base_url: requested_base_url,
        account_id,
        route_name,
        route_short_name,
        max_concurrency: requested_max_concurrency,
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
    let previous = state.config.read().await.clone();
    let mut config = previous.clone();
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
    let route_account_id = profile.official_account_id.clone();
    let account_id = account_id
        .as_deref()
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty());
    if let Some(account_id) = account_id
        && route_account_id.as_deref() != Some(account_id)
    {
        return Err("官方账号与要保存的线路不匹配".to_string());
    }
    if let Some(enabled) = requested_enabled {
        config.profiles[profile_index].enabled = enabled;
    }
    if let Some(max_concurrency) = requested_max_concurrency {
        config.profiles[profile_index].max_concurrency = max_concurrency;
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
    let selected_models = ordered_official_selection(&official_models, &requested_models)?;
    set_model_contexts(
        &mut config,
        &provider_id,
        requested_model_contexts.as_ref(),
        &official_models,
    )?;
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
    // 派生线路的运行设置同时写入账号记录。配置锁贯穿提交与回滚，防止失败
    // 时恢复旧配置覆盖另一次保存；账号更新只合并指定字段，不覆盖新凭据。
    let mut saved_account_id = None;
    let account_save = if let Some(account_id) = account_id {
        write_official_account_route_settings(
            state,
            account_id.to_string(),
            route_name.unwrap_or_default(),
            route_short_name.unwrap_or_default(),
            requested_upstream_proxy.clone().unwrap_or_default(),
            requested_base_url.clone().unwrap_or_default(),
            requested_max_concurrency,
        )
        .await
        .map(|account_id| saved_account_id = Some(account_id))
    } else if (mirrored_proxy.is_some()
        || normalized_base_url.is_some()
        || requested_max_concurrency.is_some())
        && let Some(account_id) = route_account_id
    {
        persist_official_account_runtime_settings(
            state,
            account_id,
            mirrored_proxy,
            normalized_base_url,
            requested_max_concurrency,
        )
        .await
    } else {
        Ok(())
    };
    if let Err(error) = account_save {
        let error = match save_config_to_store(state, previous).await {
            Ok(_) => error,
            Err(rollback_error) => format!("{error}；恢复保存前配置失败：{rollback_error}"),
        };
        return Err(rollback_model_catalog_after_config_save_async(catalog_refresh, error).await);
    }
    *state.config.write().await = config.clone();
    let queued_hot_reload = ModelHotReloadOutcome::queued(state, true);
    drop(_config_write_guard);
    timings.mark("saveConfigMs");
    let (response, subagent_hot_reload) = if saved_account_id.is_some() {
        let refresh = refresh_official_route_after_account_change(state).await?;
        timings.mark("modelDeliveryMs");
        let subagent_hot_reload = SubagentHotReloadOutcome::default();
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
        let hot_reload = queued_hot_reload;
        timings.mark("modelDeliveryMs");
        let subagent_hot_reload = SubagentHotReloadOutcome::default();
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

/// 模型设置命令未携带完整账号表单时，只合并明确提供的线路运行设置。
async fn persist_official_account_runtime_settings(
    state: &Arc<AppState>,
    account_id: String,
    upstream_proxy: Option<String>,
    base_url: Option<Option<String>>,
    max_concurrency: Option<u32>,
) -> Result<(), String> {
    let store = state.official_accounts();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        store.update_route_runtime_settings(&account_id, upstream_proxy, base_url, max_concurrency)
    })
    .await
    .map_err(|error| format!("保存官方账号线路运行设置任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))
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
    current_model_state_in_context(
        config,
        home,
        &model_catalog::CatalogContext::new(&config.codex_app_path),
    )
}

pub(crate) fn current_model_state_in_context(
    config: &CodeyConfig,
    home: &std::path::Path,
    context: &model_catalog::CatalogContext<'_>,
) -> Result<model_catalog::ModelSelectionState, String> {
    if !config.local_router_enabled {
        let provider = codex_provider::current_provider(home)
            .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))?;
        return native_model_state_for_provider_in_context(config, &provider, home, context);
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
    let reasoning_efforts = config.model_reasoning_efforts_for_provider(provider_id);
    let mut state = model_catalog::selection_state_in_context(
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
        Some(&reasoning_efforts),
        requested_default_model.as_deref(),
        context,
    )
    .map_err(|error| error.to_string())?;
    let synchronized = config.upstream_speed_tiers_by_provider.get(provider_id);
    if active_profile.official_siwc {
        state.synced_ultrafast_models.clear();
    } else if !config.inherits_native_speed_tiers(active_profile) {
        state.synced_ultrafast_models = synchronized
            .into_iter()
            .flatten()
            .filter(|(_, tiers)| tiers.supports_ultrafast())
            .map(|(model, _)| model.clone())
            .collect();
    }
    Ok(state)
}
