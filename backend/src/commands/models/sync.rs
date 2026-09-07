use super::*;

#[derive(Default)]
pub(crate) struct ModelHotReloadOutcome {
    pub(crate) reloaded: bool,
    pub(crate) deferred: bool,
    pub(crate) error: Option<String>,
}

impl ModelHotReloadOutcome {
    pub(crate) fn add_to_response(self, mut response: Value) -> Value {
        if let Some(object) = response.as_object_mut() {
            object.insert("modelHotReloaded".into(), Value::Bool(self.reloaded));
            if self.deferred {
                object.insert("modelHotReloadDeferred".into(), Value::Bool(true));
            }
            if let Some(error) = self.error {
                object.insert("modelHotReloadError".into(), Value::String(error));
            }
        }
        response
    }
}

pub(crate) fn add_subagent_hot_reload_to_response(
    mut response: Value,
    outcome: SubagentHotReloadOutcome,
) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "subagentConfigHotReloaded".into(),
            Value::Bool(outcome.reloaded()),
        );
        object.insert(
            "subagentConfigRepaired".into(),
            Value::Bool(outcome.repaired()),
        );
        object.insert(
            "subagentConfigHealth".into(),
            Value::String(outcome.health().to_string()),
        );
        object.insert(
            "subagentConfigRepairReasons".into(),
            Value::Array(
                outcome
                    .repair_reasons()
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        if outcome.requires_restart() {
            object.insert("restartRequired".into(), Value::Bool(true));
        }
        if let Some(error) = outcome.error() {
            object.insert(
                "subagentConfigHotReloadError".into(),
                Value::String(error.to_string()),
            );
        }
    }
    response
}

pub async fn sync_current_provider_command(state: &Arc<AppState>) -> Result<Value, String> {
    if !state.config.read().await.local_router_enabled {
        let _provider_model_sync_guard = state.provider_model_sync_lock.lock().await;
        return sync_native_current_provider_models(state, None).await;
    }
    crate::commands::prepare_routes_for_current_launch(state).await?;
    let current_provider = current_codex_provider().await?;
    let provider_status = if current_provider.official {
        let config = state.config.read().await;
        codex_provider::status_from_config(&config)
    } else {
        sync_current_third_party_provider_state(state).await?
    };
    let config = if current_provider.official {
        state.config.read().await.clone()
    } else {
        sync_provider_models_for_launch(state, true).await
    };
    let restart_required = runtime_config_requires_restart(state, &config).await;
    let model_state = current_model_state_async(&config).await?;
    let public_config = redacted_config(&config);
    Ok(json!({
        "status":"ok",
        "config":public_config,
        "providerStatus":provider_status,
        "modelState":model_state,
        "restartRequired":restart_required,
    }))
}

pub(crate) struct NativeProviderContext {
    pub(crate) provider: codex_provider::CurrentProvider,
    pub(crate) fetch_profile: Option<ProviderProfile>,
    pub(crate) route_id: String,
}

pub(crate) async fn native_provider_context(
    config: &CodeyConfig,
) -> Result<NativeProviderContext, String> {
    let config = config.clone();
    let home = codex_home().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let provider = codex_provider::current_provider(&home)
            .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))?;
        if provider.official {
            return Ok(NativeProviderContext {
                route_id: provider.id.clone(),
                provider,
                fetch_profile: None,
            });
        }
        let (projected, _) = codex_provider::sync_current_third_party_provider(&config, &home)
            .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))?;
        let fetch_profile = projected
            .active_profile()
            .ok_or_else(|| "当前 Codex 线路缺少可用的 Provider 配置".to_string())?;
        Ok(NativeProviderContext {
            route_id: fetch_profile.id.clone(),
            provider,
            fetch_profile: Some(fetch_profile),
        })
    })
    .await
    .map_err(|error| format!("读取当前 Codex 线路任务异常退出：{error}"))?
}

pub(crate) async fn sync_native_current_provider_models(
    state: &Arc<AppState>,
    expected_route: Option<(String, u64)>,
) -> Result<Value, String> {
    let previous = state.config.read().await.clone();
    if previous.local_router_enabled {
        return Err("本地路由已启用，请使用线路模型同步".to_string());
    }
    if let Some((_, expected_revision)) = expected_route.as_ref() {
        ensure_route_revision(&previous, *expected_revision)?;
    }
    let context = native_provider_context(&previous).await?;
    if let Some((expected_route_id, _)) = expected_route.as_ref()
        && expected_route_id.trim() != context.route_id
        && expected_route_id.trim() != context.provider.id
    {
        return Err("只能同步当前 Codex 线路的模型".to_string());
    }
    let visible_fetched_models = if let Some(fetch_profile) = context.fetch_profile.clone() {
        fetch_profile.validate()?;
        let fetched_models = fetch_provider_models(fetch_profile, &state.http_client)
            .await
            .map_err(|error| error.to_string())?;
        regular_route_models(fetched_models)
    } else {
        Vec::new()
    };

    let current_provider = current_codex_provider().await?;
    if current_provider != context.provider {
        return Err("同步模型期间当前 Codex 线路已变化，请重试".to_string());
    }
    let _config_write_guard = state.config_write_lock.lock().await;
    let latest = state.config.read().await.clone();
    if latest.local_router_enabled {
        return Err("同步模型期间本地路由已启用，请重试".to_string());
    }
    if latest.settings_revision != previous.settings_revision {
        return Err("Codey 设置在同步模型期间已更新，请重新载入后再操作".to_string());
    }
    if let Some((_, expected_revision)) = expected_route.as_ref() {
        ensure_route_revision(&latest, *expected_revision)?;
    }

    let mut next = latest.clone();
    if !context.provider.official {
        let mut cached_models = visible_fetched_models.clone();
        if let Some(manual_models) = next
            .manual_third_party_models_by_provider
            .get(&context.provider.id)
        {
            cached_models = preserve_selected_third_party_models(cached_models, manual_models);
        }
        next.upstream_models_by_provider
            .insert(context.provider.id.clone(), cached_models.clone());
        next.retain_1m_context_models(&context.provider.id, &cached_models);
    }
    let model_state = native_model_state_for_provider(&next, &context.provider, codex_home())?;
    let visible_models = if context.provider.official {
        let supported = model_state
            .official_models
            .iter()
            .filter(|model| model.supported)
            .map(|model| model.slug.clone())
            .collect::<Vec<_>>();
        if supported.is_empty() {
            model_state.official_model_ids.clone()
        } else {
            supported
        }
    } else {
        visible_fetched_models
    };
    reconcile_subagent_models_for_mode(&mut next, &model_state);
    next = next.normalize();
    let changed = next != latest;
    if changed {
        next.settings_revision = latest.settings_revision.saturating_add(1);
        save_config_to_store(state, &next)
            .await
            .map_err(|error| format!("保存当前线路模型同步结果失败：{error}"))?;
        *state.config.write().await = next.clone();
    }
    drop(_config_write_guard);

    let hot_reload = hot_reload_runtime_models(state, &next, &model_state).await;
    let subagent_hot_reload = if changed {
        hot_reload_runtime_subagent_config(state, &next).await
    } else {
        SubagentHotReloadOutcome::default()
    };
    let restart_required = runtime_config_requires_restart(state, &next).await;
    let provider_status = codex_provider::ProviderStatus {
        changed,
        provider: context.provider,
    };
    Ok(add_subagent_hot_reload_to_response(
        hot_reload.add_to_response(json!({
            "status": "ok",
            "config": redacted_config(&next),
            "providerStatus": provider_status,
            "models": visible_models,
            "modelState": model_state,
            "routeModelState": model_state,
            "restartRequired": restart_required,
        })),
        subagent_hot_reload,
    ))
}

pub(crate) async fn current_codex_provider() -> Result<codex_provider::CurrentProvider, String> {
    let home = codex_home().to_path_buf();
    tokio::task::spawn_blocking(move || codex_provider::current_provider(&home))
        .await
        .map_err(|error| format!("读取当前 Codex 线路任务异常退出：{error}"))?
        .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))
}

pub(crate) async fn sync_current_third_party_provider_state(
    state: &Arc<AppState>,
) -> Result<codex_provider::ProviderStatus, String> {
    let home = codex_home();
    sync_provider_state_with(state, move |config| {
        let (mut next, mut status) =
            codex_provider::sync_current_third_party_provider(&config, home)
                .map_err(|error| error.to_string())?;
        subagent_policy::reconcile_for_current_provider(&mut next, home, status.provider.official);
        next = next.normalize();
        status.changed = next != config;
        Ok((next, status))
    })
    .await
}

pub(crate) async fn sync_provider_state_with<F>(
    state: &Arc<AppState>,
    sync: F,
) -> Result<codex_provider::ProviderStatus, String>
where
    F: FnOnce(CodeyConfig) -> Result<(CodeyConfig, codex_provider::ProviderStatus), String>
        + Send
        + 'static,
{
    let previous = state.config.read().await.clone();
    ensure_local_route_config_writable(&previous)?;
    let sync_input = previous.clone();
    let sync_result = tokio::task::spawn_blocking(move || sync(sync_input)).await;
    match sync_result {
        Ok(Ok((config, status))) => {
            if !status.changed {
                return Ok(status);
            }

            let _config_write_guard = state.config_write_lock.lock().await;
            let latest = state.config.read().await.clone();
            if latest != previous {
                return Err("Codey 设置在同步线路期间已更新，已忽略过期的同步结果".to_string());
            }
            save_config_to_store(state, &config)
                .await
                .map_err(|error| format!("保存当前线路同步结果失败：{error}"))?;
            *state.config.write().await = config;
            Ok(status)
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(format!("同步当前线路任务异常退出：{error}")),
    }
}

#[cfg(test)]
pub(crate) fn config_with_current_provider_models(
    config: &CodeyConfig,
    models: Vec<String>,
) -> CodeyConfig {
    let Some(provider_id) = config.current_provider_id().map(ToString::to_string) else {
        return config.clone();
    };
    let mut next = config.clone();
    next.upstream_models_by_provider.insert(provider_id, models);
    next.normalize()
}

pub(crate) fn selected_models_not_in_upstream(
    selected_models: &[String],
    upstream_models: &[String],
) -> Vec<String> {
    let upstream_model_keys = upstream_models
        .iter()
        .map(|model| model_id::key(model))
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    selected_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| {
            !model.is_empty()
                && !upstream_model_keys.contains(&model_id::key(model))
                && seen.insert(model_id::key(model))
        })
        .map(ToString::to_string)
        .collect()
}

pub(crate) fn models_support_auto_review(models: &[String]) -> bool {
    models
        .iter()
        .any(|model| model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL))
}

pub(crate) fn regular_route_models(models: Vec<String>) -> Vec<String> {
    models
        .into_iter()
        .filter(|model| !model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL))
        .collect()
}

pub(crate) fn set_provider_auto_review_support(
    config: &mut CodeyConfig,
    provider_id: &str,
    supported: bool,
) {
    if let Some(profile) = config
        .profiles
        .iter_mut()
        .find(|profile| profile.provider_id() == provider_id && !profile.official_account)
    {
        profile.supports_auto_review = supported;
    }
}

pub(crate) fn config_with_current_provider_model_sync(
    config: &CodeyConfig,
    provider_models: Vec<String>,
    synced: bool,
    codex_home: &std::path::Path,
) -> CodeyConfig {
    let Some(provider_id) = config.current_provider_id().map(ToString::to_string) else {
        return config.clone();
    };
    let supports_auto_review = models_support_auto_review(&provider_models);
    let provider_models = regular_route_models(provider_models);
    let manual_models = if synced {
        selected_models_not_in_upstream(config.selected_models(), &provider_models)
    } else {
        preserve_selected_third_party_models(Vec::new(), config.manual_third_party_models())
    };
    let mut supported_models = if synced {
        preserve_selected_third_party_models(provider_models, config.selected_models())
    } else {
        provider_models
    };
    preserve_declared_official_models(&mut supported_models, config.declared_official_models());
    let mut next = config.clone();
    if synced {
        next.retain_1m_context_models(&provider_id, &supported_models);
        set_provider_auto_review_support(&mut next, &provider_id, supports_auto_review);
    }
    next.upstream_models_by_provider
        .insert(provider_id.clone(), supported_models);
    if manual_models.is_empty() {
        next.manual_third_party_models_by_provider
            .remove(&provider_id);
    } else {
        next.manual_third_party_models_by_provider
            .insert(provider_id, manual_models);
    }
    next = next.normalize();
    subagent_policy::reconcile_for_current_provider(&mut next, codex_home, false);
    next
}

pub(crate) fn startup_model_sync_models_or_fallback(
    models: Vec<String>,
    saved_models: Option<&[String]>,
) -> (Vec<String>, bool) {
    if models.is_empty() {
        (
            saved_models.map(<[String]>::to_vec).unwrap_or_default(),
            false,
        )
    } else {
        (models, true)
    }
}

pub(crate) fn preserve_selected_third_party_models(
    mut upstream_models: Vec<String>,
    selected_models: &[String],
) -> Vec<String> {
    preserve_selected_third_party_models_except(
        &mut upstream_models,
        selected_models,
        &HashSet::new(),
    );
    upstream_models
}

pub(crate) fn preserve_selected_third_party_models_except(
    upstream_models: &mut Vec<String>,
    selected_models: &[String],
    deleted_model_keys: &HashSet<String>,
) {
    for model in selected_models {
        let model = model.trim();
        let key = model_id::key(model);
        if model.is_empty()
            || deleted_model_keys.contains(key.as_str())
            || upstream_models
                .iter()
                .any(|existing| model_id::equal(existing, model))
        {
            continue;
        }
        upstream_models.push(model.to_string());
    }
}

pub(crate) fn preserve_declared_official_models(
    upstream_models: &mut Vec<String>,
    declared_official_models: &[String],
) {
    let official_models_by_key = model_catalog::default_official_model_slugs()
        .into_iter()
        .map(|model| (model_id::key(&model), model))
        .collect::<std::collections::HashMap<_, _>>();
    for declared_model in declared_official_models {
        let key = model_id::key(declared_model);
        let Some(official_model) = official_models_by_key.get(&key) else {
            continue;
        };
        if upstream_models
            .iter()
            .any(|existing| model_id::equal(existing, official_model))
        {
            continue;
        }
        upstream_models.push(official_model.clone());
    }
}

pub(crate) async fn fetch_provider_models(
    profile: ProviderProfile,
    http_client: &reqwest::Client,
) -> anyhow::Result<Vec<String>> {
    let home = codex_home();
    let fetch_profile = tokio::task::spawn_blocking(move || {
        codex_provider::provider_model_fetch_profile(&profile, home)
    })
    .await
    .map_err(|error| anyhow::anyhow!("解析模型源 API 配置任务异常退出：{error}"))??;
    provider_models::fetch(&fetch_profile, http_client).await
}

pub(crate) async fn sync_provider_models_for_launch(
    state: &Arc<AppState>,
    allow_third_party_sync: bool,
) -> CodeyConfig {
    let config = state.config.read().await.clone();
    if !config.local_router_enabled {
        return reconcile_current_subagent_defaults(state, None)
            .await
            .map(|(config, _)| config)
            .unwrap_or_else(|error| {
                eprintln!("启动时刷新当前 Codex 线路的子代理模型失败，沿用当前设置：{error}");
                config
            });
    }
    let Some(profile) = config.active_profile() else {
        return config;
    };
    if profile.official_account {
        return reconcile_current_subagent_defaults(state, None)
            .await
            .map(|(config, _)| config)
            .unwrap_or_else(|error| {
                eprintln!("启动时刷新官方线路模型目录失败，沿用当前设置：{error}");
                config
            });
    }
    if !allow_third_party_sync {
        return reconcile_current_subagent_defaults(state, None)
            .await
            .map(|(config, _)| config)
            .unwrap_or_else(|error| {
                eprintln!("启动时刷新已保存第三方线路模型目录失败，沿用当前设置：{error}");
                config
            });
    }
    let Some(provider_id) = config.current_provider_id().map(ToString::to_string) else {
        return config;
    };

    let (models, synced) = match tokio::time::timeout(
        STARTUP_PROVIDER_MODEL_SYNC_TIMEOUT,
        fetch_provider_models(profile.clone(), &state.http_client),
    )
    .await
    {
        Ok(Ok(models)) => {
            let fetched_model_count = models.len();
            let (provider_models, synced) =
                startup_model_sync_models_or_fallback(models, config.upstream_models_snapshot());
            if synced {
                eprintln!(
                    "启动时已从「{}」同步 {} 个上游模型",
                    profile.name, fetched_model_count
                );
            } else if config.upstream_models_snapshot().is_some() {
                eprintln!(
                    "启动时「{}」返回空模型列表，沿用已保存的模型支持配置",
                    profile.name
                );
            } else {
                eprintln!(
                    "启动时「{}」返回空模型列表，等待用户同步或手动添加线路模型",
                    profile.name
                );
            }
            (provider_models, synced)
        }
        Ok(Err(error)) => {
            let (models, synced) = startup_model_sync_models_or_fallback(
                Vec::new(),
                config.upstream_models_snapshot(),
            );
            if config.upstream_models_snapshot().is_some() {
                eprintln!(
                    "启动时同步「{}」上游模型失败，沿用已保存的模型支持配置：{error:#}",
                    profile.name
                );
            } else {
                eprintln!(
                    "启动时同步「{}」上游模型失败，未注入未经确认的模型：{error:#}",
                    profile.name
                );
            }
            (models, synced)
        }
        Err(_) => {
            let (models, synced) = startup_model_sync_models_or_fallback(
                Vec::new(),
                config.upstream_models_snapshot(),
            );
            if config.upstream_models_snapshot().is_some() {
                eprintln!(
                    "启动时同步「{}」上游模型超时，沿用已保存的模型支持配置",
                    profile.name
                );
            } else {
                eprintln!(
                    "启动时同步「{}」上游模型超时，未注入未经确认的模型",
                    profile.name
                );
            }
            (models, synced)
        }
    };
    let _config_write_guard = state.config_write_lock.lock().await;
    let latest = state.config.read().await.clone();
    if !latest.local_router_enabled {
        eprintln!("启动时同步模型期间本地路由已关闭，忽略旧线路的同步结果");
        return latest;
    }
    if latest.current_provider_id() != Some(provider_id.as_str()) {
        eprintln!("启动时同步模型期间当前线路已变化，忽略旧线路的同步结果");
        return latest;
    }
    let persistence_base = (!synced).then(|| latest.clone());
    let next = config_with_current_provider_model_sync(&latest, models, synced, codex_home());
    let committed = commit_startup_model_sync(state, latest, next, synced).await;
    drop(_config_write_guard);
    reconcile_current_subagent_defaults(state, persistence_base.as_ref())
        .await
        .map(|(config, _)| config)
        .unwrap_or_else(|error| {
            eprintln!("启动时刷新第三方线路模型目录失败，沿用当前设置：{error}");
            committed
        })
}

pub(crate) async fn commit_startup_model_sync(
    state: &Arc<AppState>,
    latest: CodeyConfig,
    next: CodeyConfig,
    synced: bool,
) -> CodeyConfig {
    if !latest.local_router_enabled {
        return latest;
    }
    if synced && let Err(error) = save_config_to_store(state, &next).await {
        eprintln!("保存启动时模型同步结果失败，本次启动沿用已持久化模型：{error:#}");
        return latest;
    }
    *state.config.write().await = next.clone();
    next
}
