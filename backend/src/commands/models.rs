use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    AppState, STARTUP_PROVIDER_MODEL_SYNC_TIMEOUT, SubagentHotReloadOutcome,
    ensure_local_route_config_writable, hot_reload_runtime_subagent_config, redacted_config,
    runtime_config_requires_restart, save_config_to_store,
};
use crate::cdp;
use crate::codex_config::codex_home;
use crate::codex_provider;
use crate::config::{
    CodeyConfig, DERIVED_OFFICIAL_PROFILE_ID, OFFICIAL_ROUTE_SHORT_NAME, ProviderProfile,
    validate_provider_profiles,
};
use crate::error_log;
use crate::local_router;
use crate::model_catalog;
use crate::model_id;
use crate::provider_models;
use crate::subagent_policy;

#[derive(Default)]
pub(super) struct ModelHotReloadOutcome {
    reloaded: bool,
    deferred: bool,
    error: Option<String>,
}

impl ModelHotReloadOutcome {
    pub(super) fn add_to_response(self, mut response: Value) -> Value {
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

fn add_subagent_hot_reload_to_response(
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
    super::prepare_routes_for_current_launch(state).await?;
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

struct NativeProviderContext {
    provider: codex_provider::CurrentProvider,
    fetch_profile: Option<ProviderProfile>,
    route_id: String,
}

async fn native_provider_context(config: &CodeyConfig) -> Result<NativeProviderContext, String> {
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
            .ok_or_else(|| "当前 Codex 线路缺少可用的 Provider 配置".to_string())?
            .clone();
        Ok(NativeProviderContext {
            route_id: fetch_profile.id.clone(),
            provider,
            fetch_profile: Some(fetch_profile),
        })
    })
    .await
    .map_err(|error| format!("读取当前 Codex 线路任务异常退出：{error}"))?
}

async fn sync_native_current_provider_models(
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
            .insert(context.provider.id.clone(), cached_models);
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

async fn current_codex_provider() -> Result<codex_provider::CurrentProvider, String> {
    let home = codex_home().to_path_buf();
    tokio::task::spawn_blocking(move || codex_provider::current_provider(&home))
        .await
        .map_err(|error| format!("读取当前 Codex 线路任务异常退出：{error}"))?
        .map_err(|error| format!("读取当前 Codex 线路失败：{error:#}"))
}

pub(super) async fn sync_current_third_party_provider_state(
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

pub(super) async fn sync_provider_state_with<F>(
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
pub(super) fn config_with_current_provider_models(
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

fn selected_models_not_in_upstream(
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

fn models_support_auto_review(models: &[String]) -> bool {
    models
        .iter()
        .any(|model| model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL))
}

fn regular_route_models(models: Vec<String>) -> Vec<String> {
    models
        .into_iter()
        .filter(|model| !model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL))
        .collect()
}

fn set_provider_auto_review_support(config: &mut CodeyConfig, provider_id: &str, supported: bool) {
    if let Some(profile) = config
        .profiles
        .iter_mut()
        .find(|profile| profile.provider_id() == provider_id && !profile.official_account)
    {
        profile.supports_auto_review = supported;
    }
}

fn config_with_current_provider_model_sync(
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

pub(super) fn startup_model_sync_models_or_fallback(
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

pub(super) fn preserve_selected_third_party_models(
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

pub(super) fn preserve_selected_third_party_models_except(
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

fn preserve_declared_official_models(
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

async fn fetch_provider_models(
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

pub(super) async fn sync_provider_models_for_launch(
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

async fn commit_startup_model_sync(
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

fn config_after_route_deletion(
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

fn ensure_route_revision(config: &CodeyConfig, expected_revision: u64) -> Result<(), String> {
    if config.settings_revision != expected_revision {
        return Err("Codey 设置已被其他操作更新，请重新载入后再操作线路".to_string());
    }
    Ok(())
}

fn config_with_provider_model_sync(
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

async fn save_native_selected_models(
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
        requested_official_models,
        requested_third_party_models,
        requested_manual_third_party_models,
        requested_deleted_third_party_models,
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

fn config_with_native_selected_models(
    config: &CodeyConfig,
    provider: &codex_provider::CurrentProvider,
    requested_official_models: Vec<String>,
    requested_third_party_models: Vec<String>,
    requested_manual_third_party_models: Vec<String>,
    requested_deleted_third_party_models: Vec<String>,
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
            validate_manual_model_selection(&official_models, &requested_official_models, &[])?;
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
    if selected.is_empty() {
        return Err("当前线路至少需要保留一个模型".to_string());
    }
    validate_deleted_models_are_manual(&existing_manual_models, &deleted_third_party_model_keys)?;
    let manual_third_party_models = validate_manual_third_party_model_sources(
        route_official_model_ids,
        &selected,
        &upstream_models,
        &existing_manual_models,
        &requested_manual_third_party_models,
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

fn validate_requested_model_list_bounds(label: &str, models: &[String]) -> Result<(), String> {
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

fn validate_regular_route_model_list(label: &str, models: &[String]) -> Result<(), String> {
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

pub(super) fn validate_manual_model_selection(
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

pub(super) fn validate_deleted_third_party_models(
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

fn validate_deleted_models_are_manual(
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

fn validate_manual_third_party_model_sources(
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

pub async fn save_official_route_models(
    state: &Arc<AppState>,
    route_id: String,
    requested_models: Vec<String>,
) -> Result<Value, String> {
    validate_requested_model_list_bounds("官方模型", &requested_models)?;
    let _config_write_guard = state.config_write_lock.lock().await;
    let mut config = state.config.read().await.clone();
    ensure_local_route_config_writable(&config)?;
    let route_id = route_id.trim();
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == route_id)
        .ok_or_else(|| "找不到要更新模型的官方账号线路".to_string())?;
    if !profile.official_account || !config.official_account_available_this_launch {
        return Err("当前线路不是本次登录可用的官方账号线路".to_string());
    }
    let provider_id = profile.provider_id().to_string();
    let official_models = model_catalog::default_official_model_slugs();
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

pub(super) async fn hot_reload_runtime_models(
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
    let expected_catalog = renderer_model_catalog_value(config, model_state);
    let expected_models = expected_catalog
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    if config.local_router_enabled {
        runtime.sync_local_router_routes(config);
    }
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

pub(super) fn current_model_state(
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
        .find(|profile| profile.id == config.active_profile_id);
    let official = active_profile.is_some_and(|profile| {
        profile.official_account && config.official_account_available_this_launch
    });
    let selected_models = if official {
        config.selected_models().to_vec()
    } else {
        config
            .current_provider_id()
            .map(|provider_id| config.enabled_route_models(provider_id))
            .unwrap_or_default()
    };
    let requested_default_model =
        active_profile.and_then(|profile| config.default_model_for_profile(profile));
    model_catalog::selection_state_with_manual_models(
        codex_home(),
        official,
        config.upstream_models_snapshot(),
        &selected_models,
        config.manual_third_party_models(),
        requested_default_model.as_deref(),
    )
    .map_err(|error| error.to_string())
}

fn native_upstream_model(config: &CodeyConfig, model: &str) -> String {
    let model = model.trim();
    config
        .runtime_model_targets()
        .into_iter()
        .find(|target| model_id::equal(&target.alias, model))
        .map(|target| target.upstream_model)
        .or_else(|| native_provider_prefixed_model(config, model))
        .unwrap_or_else(|| model.to_string())
}

fn native_provider_prefixed_model(config: &CodeyConfig, model: &str) -> Option<String> {
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

fn strip_model_provider_prefix<'a>(model: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.trim();
    model
        .get(..prefix.len())
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .and_then(|_| model.get(prefix.len()..))
        .map(str::trim)
        .filter(|suffix| !suffix.is_empty())
}

fn native_model_state_for_provider(
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

pub(super) fn reconcile_subagent_models_for_mode(
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

pub(super) async fn current_provider_status_async(
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

pub(super) async fn current_model_state_async(
    config: &CodeyConfig,
) -> Result<model_catalog::ModelSelectionState, String> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || current_model_state(&config))
        .await
        .map_err(|error| format!("读取 Codey 模型目录的任务异常退出：{error}"))?
}

async fn model_state_for_route_async(
    config: &CodeyConfig,
    route_id: &str,
) -> Result<model_catalog::ModelSelectionState, String> {
    let mut scoped = config.clone();
    scoped.active_profile_id = route_id.to_string();
    current_model_state_async(&scoped).await
}

fn current_renderer_model_catalog(config: &CodeyConfig) -> Result<Value, String> {
    let model_state = current_model_state(config)?;
    Ok(renderer_model_catalog_value(config, &model_state))
}

pub(super) async fn current_renderer_model_catalog_async(
    config: CodeyConfig,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || current_renderer_model_catalog(&config))
        .await
        .map_err(|error| format!("读取渲染进程模型目录的任务异常退出：{error}"))?
}

#[cfg(test)]
pub(super) fn provider_route_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.local_router_enabled != current.local_router_enabled
        || provider_route_snapshots(applied) != provider_route_snapshots(current)
        || websocket_transport_requires_restart(applied, current)
        || native_web_search_capability_requires_restart(applied, current)
        || remote_compaction_transport_requires_restart(applied, current)
}

pub(super) fn websocket_transport_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.runtime_supports_websockets() != current.runtime_supports_websockets()
        || applied.runtime_websocket_model_aliases() != current.runtime_websocket_model_aliases()
}

pub(super) fn remote_compaction_transport_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.runtime_supports_remote_compaction() != current.runtime_supports_remote_compaction()
}

pub(super) fn native_web_search_capability_requires_restart(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    applied.runtime_native_web_search_model_aliases()
        != current.runtime_native_web_search_model_aliases()
}

pub(super) fn runtime_supports_current_routes_for_hot_reload(
    applied: &CodeyConfig,
    current: &CodeyConfig,
) -> bool {
    if applied.local_router_enabled != current.local_router_enabled {
        return false;
    }
    if !current.local_router_enabled {
        return true;
    }
    if websocket_transport_requires_restart(applied, current)
        || native_web_search_capability_requires_restart(applied, current)
        || remote_compaction_transport_requires_restart(applied, current)
    {
        return false;
    }
    let applied = official_route_snapshots(applied);
    official_route_snapshots(current)
        .into_iter()
        .all(|(provider_id, route)| applied.get(&provider_id) == Some(&route))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProviderRouteSnapshot {
    base_url: String,
    api_key: String,
    upstream_protocol: String,
    auth_mode: String,
    official_account: bool,
    supports_remote_compaction: bool,
    supports_websockets: bool,
    supports_native_web_search: bool,
    model_request_headers: BTreeMap<String, String>,
}

fn provider_route_snapshots(config: &CodeyConfig) -> BTreeMap<String, ProviderRouteSnapshot> {
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

pub(super) fn official_route_snapshots(
    config: &CodeyConfig,
) -> BTreeMap<String, ProviderRouteSnapshot> {
    provider_route_snapshots(config)
        .into_iter()
        .filter(|(_, route)| route.official_account)
        .collect()
}

pub(super) fn renderer_model_catalog_value(
    config: &CodeyConfig,
    model_state: &model_catalog::ModelSelectionState,
) -> Value {
    if !config.local_router_enabled {
        return renderer_native_model_catalog_value(model_state);
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
            metadata
        })
        .collect::<Vec<_>>();
    let default_model = route_catalog
        .iter()
        .find(|entry| entry.is_default)
        .or_else(|| route_catalog.first())
        .map(|entry| entry.alias.clone())
        .unwrap_or_else(|| model_state.default_model.clone());
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
        "sources": [],
        "responses_api": {
            "status": "unknown",
            "message": ""
        }
    })
}

fn renderer_native_model_catalog_value(model_state: &model_catalog::ModelSelectionState) -> Value {
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
struct RendererRouteModelEntry {
    alias: String,
    provider_id: String,
    request_provider_id: String,
    request_model: String,
    official_account: bool,
    route_name: String,
    route_prefix: String,
    model: String,
    supported_reasoning_efforts: Vec<String>,
    default_reasoning_effort: String,
    is_default: bool,
}

fn renderer_route_model_catalog(
    config: &CodeyConfig,
    active_model_state: &model_catalog::ModelSelectionState,
) -> Vec<RendererRouteModelEntry> {
    let mut entries = Vec::new();
    let mut aliases = HashSet::new();
    for profile in &config.profiles {
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

fn route_model_alias(provider_id: &str, model: &str, aliases: &mut HashSet<String>) -> String {
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

pub(super) fn should_refresh_model_catalog(
    model_state: &model_catalog::ModelSelectionState,
) -> bool {
    !model_state.official_models.is_empty() || !model_state.third_party_models.is_empty()
}

struct ModelCatalogRefresh {
    fallback: bool,
    snapshot: model_catalog::CatalogSnapshot,
}

fn refresh_model_catalog_or_fallback(config: &CodeyConfig) -> Result<ModelCatalogRefresh, String> {
    let home = codex_home();
    let snapshot = model_catalog::snapshot(home).map_err(|error| error.to_string())?;
    let native_web_search_models = config.runtime_native_web_search_model_aliases();
    let result = model_catalog_fallback(
        try_refresh_model_catalog(config),
        home,
        &native_web_search_models,
    );
    match result {
        Ok(fallback) => Ok(ModelCatalogRefresh { fallback, snapshot }),
        Err(error) => Err(rollback_model_catalog_snapshot(snapshot, error)),
    }
}

async fn refreshed_model_state_async(
    config: &CodeyConfig,
    refresh_only_when_populated: bool,
) -> Result<
    (
        Option<ModelCatalogRefresh>,
        model_catalog::ModelSelectionState,
    ),
    String,
> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        let should_refresh = if refresh_only_when_populated {
            should_refresh_model_catalog(&current_model_state(&config)?)
        } else {
            true
        };
        let refresh = should_refresh
            .then(|| refresh_model_catalog_or_fallback(&config))
            .transpose()?;
        match current_model_state(&config) {
            Ok(model_state) => Ok((refresh, model_state)),
            Err(error) => Err(rollback_model_catalog_after_config_save(refresh, error)),
        }
    })
    .await
    .map_err(|error| format!("刷新 Codey 模型目录的任务异常退出：{error}"))?
}

async fn reconcile_current_subagent_defaults(
    state: &Arc<AppState>,
    persistence_base: Option<&CodeyConfig>,
) -> Result<(CodeyConfig, bool), String> {
    let _config_write_guard = state.config_write_lock.lock().await;
    let current = state.config.read().await.clone();
    let (catalog_refresh, model_state) = if current.local_router_enabled {
        refreshed_model_state_async(&current, false).await?
    } else {
        (None, current_model_state_async(&current).await?)
    };
    let mut next = current.clone();
    reconcile_subagent_models_for_mode(&mut next, &model_state);
    next = next.normalize();
    if next == current {
        return Ok((current, false));
    }
    let persisted = persistence_base.map_or_else(
        || next.clone(),
        |base| config_with_reconciled_subagent_defaults(base, &next),
    );
    if let Err(error) = save_config_to_store(state, &persisted).await {
        return Err(rollback_model_catalog_after_config_save_async(catalog_refresh, error).await);
    }
    *state.config.write().await = next.clone();
    Ok((next, true))
}

fn config_with_reconciled_subagent_defaults(
    persistence_base: &CodeyConfig,
    reconciled: &CodeyConfig,
) -> CodeyConfig {
    let mut persisted = persistence_base.clone();
    persisted.subagent_optimization = reconciled.subagent_optimization;
    persisted
        .subagent_model
        .clone_from(&reconciled.subagent_model);
    persisted
        .subagent_reasoning_effort
        .clone_from(&reconciled.subagent_reasoning_effort);
    persisted
        .subagent_roles
        .clone_from(&reconciled.subagent_roles);
    persisted.normalize()
}

fn rollback_model_catalog_after_config_save(
    refresh: Option<ModelCatalogRefresh>,
    error: String,
) -> String {
    match refresh {
        Some(refresh) => rollback_model_catalog_snapshot(refresh.snapshot, error),
        None => error,
    }
}

async fn rollback_model_catalog_after_config_save_async(
    refresh: Option<ModelCatalogRefresh>,
    error: String,
) -> String {
    let primary_error = error.clone();
    tokio::task::spawn_blocking(move || rollback_model_catalog_after_config_save(refresh, error))
        .await
        .unwrap_or_else(|join_error| {
            format!("{primary_error}；回滚 Codey 模型目录的任务异常退出：{join_error}")
        })
}

fn rollback_model_catalog_snapshot(
    snapshot: model_catalog::CatalogSnapshot,
    error: String,
) -> String {
    match model_catalog::restore_snapshot(snapshot) {
        Ok(()) => error,
        Err(rollback_error) => {
            format!("{error}；回滚 Codey 模型目录也失败：{rollback_error:#}")
        }
    }
}

fn model_catalog_fallback(
    result: anyhow::Result<()>,
    home: &std::path::Path,
    native_web_search_models: &[String],
) -> Result<bool, String> {
    match result {
        Ok(()) => Ok(false),
        Err(error) if model_catalog::is_runtime_model_cache_unavailable(&error) => {
            model_catalog::prepare_cached_catalog_for_native_web_search(
                home,
                native_web_search_models,
            )
            .map(|available| !available)
            .map_err(|fallback_error| fallback_error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn try_refresh_model_catalog(config: &CodeyConfig) -> anyhow::Result<()> {
    let has_third_party_route = config.has_third_party_route();
    let (upstream_models, selected_models) = config.runtime_catalog_models();
    let websocket_models = config.runtime_websocket_model_aliases();
    let native_web_search_models = config.runtime_native_web_search_model_aliases();
    model_catalog::refresh_for_provider_with_capabilities(
        codex_home(),
        config.official_account_available_this_launch && !has_third_party_route,
        has_third_party_route.then_some(upstream_models).as_deref(),
        &selected_models,
        &websocket_models,
        &native_web_search_models,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_route(id: &str, model: Option<&str>) -> ProviderProfile {
        let mut profile = ProviderProfile::new(id);
        profile.id = id.to_string();
        profile.base_url = format!("https://{id}.example/v1");
        profile.api_key = format!("{id}-key");
        profile.api_key_configured = true;
        if model.is_none() {
            profile.name = format!("{id}-without-models");
        }
        profile.normalize();
        profile
    }

    #[test]
    fn native_model_state_and_subagent_defaults_follow_only_the_current_provider() {
        let home = tempfile::tempdir().unwrap();
        let route_a = configured_route("route-a", Some("model-a"));
        let route_b = configured_route("route-b", Some("model-b"));
        let mut config = CodeyConfig {
            local_router_enabled: false,
            active_profile_id: route_b.id.clone(),
            profiles: vec![route_a, route_b],
            selected_models_by_provider: BTreeMap::from([
                ("route-a".into(), vec!["model-a".into()]),
                ("route-b".into(), vec!["model-b".into()]),
            ]),
            upstream_models_by_provider: BTreeMap::from([
                ("route-a".into(), vec!["model-a".into()]),
                ("route-b".into(), vec!["model-b".into()]),
            ]),
            subagent_model: "route-b/model-b".into(),
            subagent_roles: crate::config::uniform_subagent_roles("route-b/model-b", "high"),
            ..CodeyConfig::default()
        }
        .normalize();
        let provider = codex_provider::CurrentProvider {
            id: "route-a".into(),
            name: "Current route".into(),
            official: false,
            supports_remote_compaction: false,
            base_url: "https://route-a.example/v1".into(),
        };

        let state = native_model_state_for_provider(&config, &provider, home.path()).unwrap();

        assert_eq!(state.third_party_models, ["model-a"]);
        assert_eq!(state.upstream_models, ["model-a"]);
        assert!(
            !state
                .third_party_models
                .iter()
                .any(|model| model == "model-b")
        );

        reconcile_subagent_models_for_mode(&mut config, &state);
        assert_eq!(config.subagent_model, "model-a");
        assert!(
            config
                .subagent_roles
                .values()
                .all(|selection| selection.model == "model-a")
        );
    }

    #[test]
    fn native_model_selection_updates_current_provider_models_without_route_edits() {
        let home = tempfile::tempdir().unwrap();
        let route = configured_route("route-a", None);
        let provider = codex_provider::CurrentProvider {
            id: "route-a".into(),
            name: "Current route".into(),
            official: false,
            supports_remote_compaction: false,
            base_url: route.base_url.clone(),
        };
        let config = CodeyConfig {
            local_router_enabled: false,
            active_profile_id: route.id.clone(),
            profiles: vec![route.clone()],
            upstream_models_by_provider: BTreeMap::from([(
                "route-a".into(),
                vec!["model-a".into(), "model-b".into()],
            )]),
            subagent_model: "route-a/model-a".into(),
            subagent_roles: crate::config::uniform_subagent_roles("route-a/model-a", "high"),
            ..CodeyConfig::default()
        }
        .normalize();

        let mut selected = config_with_native_selected_models(
            &config,
            &provider,
            Vec::new(),
            vec!["model-b".into()],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let state = native_model_state_for_provider(&selected, &provider, home.path()).unwrap();
        reconcile_subagent_models_for_mode(&mut selected, &state);
        selected = selected.normalize();

        assert_eq!(selected.profiles, config.profiles);
        assert_eq!(
            selected.upstream_models_by_provider["route-a"],
            ["model-a", "model-b"]
        );
        assert_eq!(selected.selected_models_by_provider["route-a"], ["model-b"]);
        assert_eq!(state.third_party_models, ["model-b"]);
        assert_eq!(selected.subagent_model, "model-b");
    }

    #[test]
    fn native_official_model_selection_keeps_selection_official_only() {
        let provider = codex_provider::CurrentProvider {
            id: "openai".into(),
            name: "OpenAI".into(),
            official: true,
            supports_remote_compaction: true,
            base_url: String::new(),
        };
        let config = CodeyConfig {
            local_router_enabled: false,
            ..CodeyConfig::default()
        }
        .normalize();

        let selected = config_with_native_selected_models(
            &config,
            &provider,
            vec!["gpt-5.6-sol".into()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(
            selected.selected_models_by_provider["openai"],
            ["gpt-5.6-sol"]
        );
        let home = tempfile::tempdir().unwrap();
        let model_state =
            native_model_state_for_provider(&selected, &provider, home.path()).unwrap();
        let catalog = renderer_model_catalog_value(&selected, &model_state);
        assert_eq!(catalog["native_selection_only"], true);
        assert_eq!(catalog["models"], json!(["gpt-5.6-sol"]));
        assert!(catalog.get("model_provider").is_none());
        assert!(
            catalog["model_metadata"][0]
                .get("route_provider_id")
                .is_none()
        );
        assert!(
            config_with_native_selected_models(
                &config,
                &provider,
                vec!["gpt-5.6-sol".into()],
                vec!["custom-model".into()],
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .contains("官方线路不支持添加第三方模型")
        );
    }

    #[test]
    fn native_renderer_catalog_and_hot_reload_do_not_require_local_routes() {
        let config = CodeyConfig {
            local_router_enabled: false,
            ..CodeyConfig::default()
        };
        let model_state = model_catalog::ModelSelectionState {
            third_party_models: vec!["org/model-a".into()],
            upstream_models: vec!["org/model-a".into(), "unchecked-model".into()],
            default_model: "org/model-a".into(),
            ..Default::default()
        };
        let catalog = renderer_model_catalog_value(&config, &model_state);
        assert_eq!(catalog["models"], json!(["org/model-a"]));
        assert!(catalog["model_metadata"][0].get("provider_id").is_none());
        assert!(runtime_supports_current_routes_for_hot_reload(
            &config, &config
        ));
        let enabled = CodeyConfig {
            local_router_enabled: true,
            ..config.clone()
        };
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &enabled, &config
        ));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &config, &enabled
        ));
    }

    #[test]
    fn native_third_party_gpt_selection_keeps_raw_ids_and_does_not_restore_unchecked_models() {
        let home = tempfile::tempdir().unwrap();
        let provider = codex_provider::CurrentProvider {
            id: "external-provider".into(),
            name: "Native provider".into(),
            official: false,
            supports_remote_compaction: false,
            base_url: "https://example.invalid/v1".into(),
        };
        let mut config = CodeyConfig {
            local_router_enabled: false,
            ..CodeyConfig::default()
        }
        .normalize();
        config.upstream_models_by_provider.insert(
            provider.id.clone(),
            vec!["gpt-5.5".into(), "claude-sonnet-4-5".into()],
        );
        let selected = config_with_native_selected_models(
            &config,
            &provider,
            Vec::new(),
            vec!["gpt-5.5".into()],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let state = native_model_state_for_provider(&selected, &provider, home.path()).unwrap();
        assert_eq!(selected.profiles, config.profiles);
        assert_eq!(
            selected.selected_models_by_provider[&provider.id],
            ["gpt-5.5"]
        );
        assert_eq!(state.third_party_models, ["gpt-5.5"]);
        assert_eq!(state.upstream_models, ["gpt-5.5", "claude-sonnet-4-5"]);
        assert!(state.official_models.is_empty());
    }

    #[test]
    fn native_model_conversion_uses_synced_upstream_models_without_enabled_targets() {
        let route = configured_route("route-a", None);
        let config = CodeyConfig {
            local_router_enabled: false,
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            upstream_models_by_provider: BTreeMap::from([(
                "route-a".into(),
                vec!["vendor/model".into()],
            )]),
            ..CodeyConfig::default()
        };

        assert_eq!(
            native_upstream_model(&config, "route-a/vendor/model"),
            "vendor/model"
        );
    }

    #[test]
    fn route_mutations_reject_a_stale_settings_revision() {
        let config = CodeyConfig {
            settings_revision: 9,
            ..CodeyConfig::default()
        };

        assert!(ensure_route_revision(&config, 9).is_ok());
        assert!(
            ensure_route_revision(&config, 8)
                .unwrap_err()
                .contains("重新载入")
        );
    }

    #[test]
    fn deleting_a_route_falls_back_global_default_and_dependent_subagent_roles() {
        let route_a = configured_route("route-a", Some("model-a"));
        let route_b = configured_route("route-b", Some("model-b"));
        let mut roles = crate::config::uniform_subagent_roles("route-b/model-b", "high");
        roles.insert(
            crate::config::SUBAGENT_ROLE_WORKER.into(),
            crate::config::SubagentRoleConfig::new("route-a/model-a", "medium"),
        );
        let previous = CodeyConfig {
            settings_revision: 7,
            active_profile_id: route_b.id.clone(),
            profiles: vec![route_a, route_b],
            selected_models_by_provider: BTreeMap::from([
                ("route-a".into(), vec!["model-a".into()]),
                ("route-b".into(), vec!["model-b".into()]),
            ]),
            manual_third_party_models_by_provider: BTreeMap::from([(
                "route-b".into(),
                vec!["model-b".into()],
            )]),
            declared_official_models_by_provider: BTreeMap::from([(
                "route-b".into(),
                vec!["gpt-5.6-terra".into()],
            )]),
            upstream_models_by_provider: BTreeMap::from([(
                "route-b".into(),
                vec!["model-b".into(), "gpt-5.6-terra".into()],
            )]),
            default_model: "route-b/model-b".into(),
            subagent_model: "route-b/model-b".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: roles,
            ..CodeyConfig::default()
        }
        .normalize();

        let next = config_after_route_deletion(&previous, "route-b").unwrap();

        assert_eq!(next.settings_revision, 8);
        assert_eq!(next.active_profile_id, "route-a");
        assert_eq!(next.default_model, "route-a/model-a");
        assert_eq!(next.subagent_model, "route-a/model-a");
        assert!(
            next.subagent_roles
                .values()
                .all(|selection| selection.model == "route-a/model-a")
        );
        assert!(!next.selected_models_by_provider.contains_key("route-b"));
        assert!(
            !next
                .manual_third_party_models_by_provider
                .contains_key("route-b")
        );
        assert!(
            !next
                .declared_official_models_by_provider
                .contains_key("route-b")
        );
        assert!(!next.upstream_models_by_provider.contains_key("route-b"));

        let valid_aliases = next
            .runtime_model_targets()
            .into_iter()
            .map(|target| target.alias)
            .collect::<HashSet<_>>();
        assert!(valid_aliases.contains(&next.default_model));
        assert!(
            next.subagent_roles
                .values()
                .all(|selection| valid_aliases.contains(&selection.model))
        );
    }

    #[test]
    fn deleting_a_route_used_only_by_one_role_falls_back_to_the_existing_default() {
        let route_a = configured_route("route-a", Some("model-a"));
        let route_b = configured_route("route-b", Some("model-b"));
        let mut roles = crate::config::uniform_subagent_roles("route-a/model-a", "high");
        roles.insert(
            crate::config::SUBAGENT_ROLE_QUICK_SCAN.into(),
            crate::config::SubagentRoleConfig::new("route-b/model-b", "low"),
        );
        let previous = CodeyConfig {
            active_profile_id: route_a.id.clone(),
            profiles: vec![route_a, route_b],
            selected_models_by_provider: BTreeMap::from([
                ("route-a".into(), vec!["model-a".into()]),
                ("route-b".into(), vec!["model-b".into()]),
            ]),
            default_model: "route-a/model-a".into(),
            subagent_model: "route-a/model-a".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: roles,
            ..CodeyConfig::default()
        }
        .normalize();

        let next = config_after_route_deletion(&previous, "route-b").unwrap();

        assert_eq!(next.default_model, "route-a/model-a");
        assert_eq!(next.subagent_model, "route-a/model-a");
        assert_eq!(
            next.subagent_roles[crate::config::SUBAGENT_ROLE_QUICK_SCAN].model,
            "route-a/model-a"
        );
        assert_eq!(
            next.subagent_roles[crate::config::SUBAGENT_ROLE_QUICK_SCAN].reasoning_effort,
            "low"
        );
    }

    #[test]
    fn deleting_an_unrelated_route_preserves_valid_global_model_references() {
        let route_a = configured_route("route-a", Some("model-a"));
        let route_b = configured_route("route-b", Some("model-b"));
        let previous = CodeyConfig {
            active_profile_id: route_a.id.clone(),
            profiles: vec![route_a, route_b],
            selected_models_by_provider: BTreeMap::from([
                ("route-a".into(), vec!["model-a".into()]),
                ("route-b".into(), vec!["model-b".into()]),
            ]),
            default_model: "route-a/model-a".into(),
            subagent_model: "route-a/model-a".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: crate::config::uniform_subagent_roles("route-a/model-a", "high"),
            ..CodeyConfig::default()
        }
        .normalize();

        let next = config_after_route_deletion(&previous, "route-b").unwrap();

        assert_eq!(next.default_model, "route-a/model-a");
        assert_eq!(next.subagent_model, "route-a/model-a");
        assert!(
            next.subagent_roles
                .values()
                .all(|selection| selection.model == "route-a/model-a")
        );
    }

    #[test]
    fn deleting_the_only_modeled_route_clears_stale_default_and_uses_product_subagent_default() {
        let route_a = configured_route("route-a", None);
        let route_b = configured_route("route-b", Some("model-b"));
        let previous = CodeyConfig {
            active_profile_id: route_b.id.clone(),
            profiles: vec![route_a, route_b],
            selected_models_by_provider: BTreeMap::from([(
                "route-b".into(),
                vec!["model-b".into()],
            )]),
            default_model: "route-b/model-b".into(),
            subagent_model: "route-b/model-b".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: crate::config::uniform_subagent_roles("route-b/model-b", "high"),
            ..CodeyConfig::default()
        }
        .normalize();

        let next = config_after_route_deletion(&previous, "route-b").unwrap();

        assert!(next.default_model.is_empty());
        assert_eq!(next.subagent_model, crate::config::DEFAULT_SUBAGENT_MODEL);
        assert!(
            next.subagent_roles
                .values()
                .all(|selection| { selection.model == crate::config::DEFAULT_SUBAGENT_MODEL })
        );
    }

    #[test]
    fn syncing_one_route_does_not_change_another_routes_models() {
        let home = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig::default();
        config
            .upstream_models_by_provider
            .insert("route-a".into(), vec!["route-a-model".into()]);
        config
            .selected_models_by_provider
            .insert("route-a".into(), vec!["route-a-model".into()]);
        config
            .selected_models_by_provider
            .insert("route-b".into(), vec!["route-b-manual".into()]);

        let synced = config_with_provider_model_sync(
            &config,
            "route-b",
            vec!["route-b-upstream".into()],
            home.path(),
        );

        assert_eq!(
            synced.upstream_models_by_provider["route-a"],
            ["route-a-model"]
        );
        assert_eq!(
            synced.selected_models_by_provider["route-a"],
            ["route-a-model"]
        );
        assert_eq!(
            synced.upstream_models_by_provider["route-b"],
            ["route-b-upstream", "route-b-manual"]
        );
    }

    #[test]
    fn requested_model_lists_enforce_count_and_id_limits() {
        let too_many = (0..=provider_models::MAX_PROVIDER_MODELS)
            .map(|index| format!("model-{index}"))
            .collect::<Vec<_>>();
        assert!(
            validate_requested_model_list_bounds("其他模型", &too_many)
                .unwrap_err()
                .contains("数量超过安全上限")
        );

        let too_long = vec!["m".repeat(provider_models::MAX_PROVIDER_MODEL_ID_BYTES + 1)];
        assert!(
            validate_requested_model_list_bounds("其他模型", &too_long)
                .unwrap_err()
                .contains("ID 超过安全上限")
        );
    }

    #[test]
    fn manual_model_selection_keeps_first_case_insensitive_duplicate() {
        let official = model_catalog::default_official_model_slugs();
        let (_, selected) = validate_manual_model_selection(
            &official,
            &[],
            &[
                "provider-a".into(),
                "provider-a".into(),
                "Provider-A".into(),
                "provider-b".into(),
            ],
        )
        .unwrap();

        assert_eq!(selected, ["provider-a", "provider-b"]);
    }

    #[test]
    fn model_changes_accept_only_the_known_builtin_catalog_fallback() {
        let home = tempfile::tempdir().unwrap();
        let missing_cache =
            model_catalog::refresh_for_provider(home.path(), false, Some(&[]), &[]).unwrap_err();

        assert!(model_catalog_fallback(Err(missing_cache), home.path(), &[]).unwrap());
        assert!(!model_catalog_fallback(Ok(()), home.path(), &[]).unwrap());
        assert_eq!(
            model_catalog_fallback(Err(anyhow::anyhow!("模型目录写入失败")), home.path(), &[],)
                .unwrap_err(),
            "模型目录写入失败"
        );
    }

    #[test]
    fn synced_models_are_not_marked_as_manual_sources() {
        let official = model_catalog::default_official_model_slugs();

        let manual = validate_manual_third_party_model_sources(
            &official,
            &["provider-synced".into(), "provider-manual".into()],
            &["provider-synced".into()],
            &[],
            &["provider-synced".into(), "provider-manual".into()],
        )
        .unwrap();

        assert_eq!(manual, ["provider-manual"]);
    }

    #[test]
    fn deleted_model_must_be_a_saved_manual_source() {
        let deleted = ["provider-synced".to_string()]
            .into_iter()
            .collect::<HashSet<_>>();

        let error =
            validate_deleted_models_are_manual(&["provider-manual".into()], &deleted).unwrap_err();

        assert!(error.contains("不是手动添加的其他模型"));
    }

    #[test]
    fn provider_sync_reclassifies_old_selected_models_by_the_raw_upstream_list() {
        let home = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config.selected_models_by_provider.insert(
            provider_id.clone(),
            vec!["provider-synced".into(), "provider-manual".into()],
        );

        let synced = config_with_current_provider_model_sync(
            &config,
            vec!["provider-synced".into()],
            true,
            home.path(),
        );

        assert_eq!(
            synced.manual_third_party_models_by_provider[&provider_id],
            ["provider-manual"]
        );
        assert_eq!(
            synced.upstream_models_by_provider[&provider_id],
            ["provider-synced", "provider-manual"]
        );
    }

    #[test]
    fn successful_provider_sync_replaces_auto_review_capability() {
        let home = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();

        let supported = config_with_current_provider_model_sync(
            &config,
            vec![
                "provider-model".into(),
                local_router::CODEX_AUTO_REVIEW_MODEL.into(),
            ],
            true,
            home.path(),
        );
        assert!(supported.profiles[0].supports_auto_review);
        assert_eq!(
            supported.upstream_models_by_provider[&provider_id],
            ["provider-model"]
        );

        config = supported;
        let unsupported = config_with_current_provider_model_sync(
            &config,
            vec!["provider-model".into()],
            true,
            home.path(),
        );
        assert!(!unsupported.profiles[0].supports_auto_review);
    }

    #[test]
    fn failed_provider_sync_preserves_auto_review_capability() {
        let home = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config.profiles[0].supports_auto_review = true;
        config
            .upstream_models_by_provider
            .insert(provider_id, vec!["saved-model".into()]);

        let fallback = config_with_current_provider_model_sync(
            &config,
            vec!["saved-model".into()],
            false,
            home.path(),
        );

        assert!(fallback.profiles[0].supports_auto_review);
    }

    #[test]
    fn auto_review_cannot_be_saved_as_a_regular_model() {
        let error = validate_regular_route_model_list(
            "其他模型",
            &[local_router::CODEX_AUTO_REVIEW_MODEL.into()],
        )
        .unwrap_err();

        assert!(error.contains("Auto Review 线路能力开关"));
    }

    #[test]
    fn provider_sync_preserves_only_user_declared_official_models() {
        let home = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config.upstream_models_by_provider.insert(
            provider_id.clone(),
            vec!["gpt-5.6-sol".into(), "gpt-5.6-luna".into()],
        );
        config
            .declared_official_models_by_provider
            .insert(provider_id.clone(), vec![" GPT-5.6-SOL ".into()]);

        let synced = config_with_current_provider_model_sync(
            &config,
            vec!["provider-custom-model".into()],
            true,
            home.path(),
        );

        assert_eq!(
            synced.upstream_models_by_provider[&provider_id],
            ["provider-custom-model", "gpt-5.6-sol"]
        );
    }

    #[test]
    fn provider_model_refresh_preserves_subagent_settings_when_no_replacement_is_selected() {
        let home = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig {
            subagent_optimization: true,
            subagent_model: "gpt-5.6-sol".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: crate::config::uniform_subagent_roles("gpt-5.6-sol", "high"),
            ..CodeyConfig::default()
        };
        let provider_id = config.current_provider_id().unwrap().to_string();
        config
            .upstream_models_by_provider
            .insert(provider_id, vec!["gpt-5.6-sol".into()]);

        let synced = config_with_current_provider_model_sync(
            &config,
            vec!["provider-custom-model".into()],
            true,
            home.path(),
        );

        assert!(synced.subagent_optimization);
        assert_eq!(synced.subagent_model, "gpt-5.6-sol");
        assert_eq!(synced.subagent_reasoning_effort, "high");
    }

    #[tokio::test]
    async fn startup_model_sync_does_not_publish_memory_when_persistence_fails() {
        let directory = tempfile::tempdir().unwrap();
        let latest = CodeyConfig::default();
        let mut next = latest.clone();
        let provider_id = next.current_provider_id().unwrap().to_string();
        next.upstream_models_by_provider
            .insert(provider_id, vec!["provider-new".into()]);
        let state = Arc::new(AppState {
            store: crate::config::ConfigStore::new(directory.path()),
            config: tokio::sync::RwLock::new(latest.clone()),
            ..AppState::default()
        });

        let committed = commit_startup_model_sync(&state, latest.clone(), next, true).await;

        assert_eq!(committed, latest);
        assert_eq!(*state.config.read().await, latest);
    }

    #[tokio::test]
    async fn disabled_local_router_discards_a_stale_startup_model_sync_commit() {
        let directory = tempfile::tempdir().unwrap();
        let latest = CodeyConfig {
            local_router_enabled: false,
            ..CodeyConfig::default()
        };
        let mut next = latest.clone();
        let provider_id = next.current_provider_id().unwrap().to_string();
        next.upstream_models_by_provider
            .insert(provider_id, vec!["must-not-persist".into()]);
        let state = Arc::new(AppState {
            store: crate::config::ConfigStore::new(directory.path().join("config.json")),
            config: tokio::sync::RwLock::new(latest.clone()),
            ..AppState::default()
        });

        let committed = commit_startup_model_sync(&state, latest.clone(), next, true).await;

        assert_eq!(committed, latest);
        assert_eq!(*state.config.read().await, latest);
        assert!(!state.store.path().exists());
    }

    #[test]
    fn startup_fallback_persists_only_reconciled_subagent_defaults() {
        let mut persisted = CodeyConfig {
            subagent_optimization: true,
            subagent_model: "gpt-5.6-luna".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: crate::config::uniform_subagent_roles("gpt-5.6-luna", "high"),
            ..CodeyConfig::default()
        };
        let provider_id = persisted.current_provider_id().unwrap().to_string();
        persisted
            .upstream_models_by_provider
            .insert(provider_id.clone(), vec!["saved-model".into()]);

        let mut runtime_fallback = persisted.clone();
        runtime_fallback
            .upstream_models_by_provider
            .insert(provider_id.clone(), vec!["fallback-model".into()]);
        runtime_fallback.subagent_model = crate::config::DEFAULT_SUBAGENT_MODEL.into();
        runtime_fallback.subagent_roles.insert(
            crate::config::SUBAGENT_ROLE_DEFAULT.into(),
            crate::config::SubagentRoleConfig::new(crate::config::DEFAULT_SUBAGENT_MODEL, "high"),
        );

        let next = config_with_reconciled_subagent_defaults(&persisted, &runtime_fallback);

        assert_eq!(
            next.upstream_models_by_provider.get(&provider_id),
            Some(&vec!["saved-model".into()])
        );
        assert_eq!(next.subagent_model, crate::config::DEFAULT_SUBAGENT_MODEL);
        assert_eq!(next.subagent_reasoning_effort, "high");
    }

    #[test]
    fn renderer_catalog_routes_every_model_through_the_codey_router_carrier() {
        let mut official = ProviderProfile::new("官方线路");
        official.id = "official-profile".into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();

        let mut relay = ProviderProfile::new("中转线路");
        relay.id = "relay".into();
        relay.base_url = "https://relay.example/v1".into();
        relay.api_key = "relay-key".into();
        relay.normalize();

        let mut config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official, relay],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        };
        config
            .selected_models_by_provider
            .insert("relay".into(), vec!["gpt-5.6-sol".into()]);
        config.default_model = "relay/gpt-5.6-sol".into();
        config = config.normalize();

        let model_state = model_catalog::ModelSelectionState {
            official_models: vec![model_catalog::OfficialModelAvailability {
                slug: "gpt-5.6-sol".into(),
                display_name: "GPT-5.6 Sol".into(),
                supported: true,
                supported_reasoning_efforts: vec!["medium".into()],
                default_reasoning_effort: "medium".into(),
            }],
            official_model_ids: vec!["gpt-5.6-sol".into()],
            third_party_models: Vec::new(),
            third_party_model_metadata: Vec::new(),
            manual_third_party_models: Vec::new(),
            upstream_models: Vec::new(),
            default_model: "gpt-5.6-sol".into(),
        };

        let catalog = renderer_model_catalog_value(&config, &model_state);
        let model_names = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model.as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(model_names.contains(&"gpt-5.6-sol"));
        assert!(model_names.contains(&"relay/gpt-5.6-sol"));
        assert_eq!(catalog["default_model"].as_str(), Some("relay/gpt-5.6-sol"));
        assert_eq!(
            catalog["model_provider"].as_str(),
            Some(local_router::ROUTER_PROVIDER_ID)
        );
        assert_eq!(catalog["provider_name"].as_str(), Some("中转线路"));

        let official_metadata = catalog["model_metadata"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["model"].as_str() == Some("gpt-5.6-sol"))
            .unwrap();
        assert_eq!(
            official_metadata["display_name"].as_str(),
            Some("[官] gpt-5.6-sol")
        );
        assert_eq!(official_metadata["route_name"].as_str(), Some("官方线路"));
        assert_eq!(official_metadata["route_prefix"].as_str(), Some("官"));
        assert_eq!(
            official_metadata["provider_id"].as_str(),
            Some(local_router::ROUTER_PROVIDER_ID)
        );
        assert_eq!(
            official_metadata["source_model"].as_str(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            official_metadata["route_provider_id"].as_str(),
            Some("openai")
        );
        assert_eq!(official_metadata["official_account"], true);
        assert_eq!(
            official_metadata["upstream_model"].as_str(),
            Some("gpt-5.6-sol")
        );
        let relay_metadata = catalog["model_metadata"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["model"].as_str() == Some("relay/gpt-5.6-sol"))
            .unwrap();
        assert_eq!(relay_metadata["official_account"], false);
    }

    #[test]
    fn renderer_catalog_keeps_multi_segment_models_from_a_non_current_route() {
        let active = configured_route("active-route", Some("active-model"));
        let mut tokenrouter = configured_route("tokenrouter", Some("z-ai/glm-5.3-free"));
        tokenrouter.name = "TokenRouter".into();
        tokenrouter.short_name = "tokenrouter".into();

        let config = CodeyConfig {
            active_profile_id: active.id.clone(),
            profiles: vec![active, tokenrouter],
            selected_models_by_provider: BTreeMap::from([
                ("active-route".into(), vec!["active-model".into()]),
                ("tokenrouter".into(), vec!["z-ai/glm-5.3-free".into()]),
            ]),
            upstream_models_by_provider: BTreeMap::from([
                ("active-route".into(), vec!["active-model".into()]),
                ("tokenrouter".into(), vec!["z-ai/glm-5.3-free".into()]),
            ]),
            ..CodeyConfig::default()
        }
        .normalize();
        let active_model_state = model_catalog::ModelSelectionState {
            third_party_models: vec!["active-model".into()],
            upstream_models: vec!["active-model".into()],
            default_model: "active-model".into(),
            ..Default::default()
        };

        let catalog = renderer_model_catalog_value(&config, &active_model_state);
        assert!(
            catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model.as_str() == Some("tokenrouter/z-ai/glm-5.3-free"))
        );
        let tokenrouter_metadata = catalog["model_metadata"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["model"].as_str() == Some("tokenrouter/z-ai/glm-5.3-free"))
            .unwrap();
        assert_eq!(
            tokenrouter_metadata["source_model"].as_str(),
            Some("z-ai/glm-5.3-free")
        );
        assert_eq!(
            tokenrouter_metadata["route_provider_id"].as_str(),
            Some("tokenrouter")
        );
    }

    #[test]
    fn provider_route_restart_detection_ignores_model_only_changes() {
        let mut route_a = crate::config::ProviderProfile::new("Route A");
        route_a.id = "route-a".into();
        route_a.base_url = "https://route-a.example/v1".into();
        route_a.api_key = "route-a-secret".into();
        let mut route_b = crate::config::ProviderProfile::new("Route B");
        route_b.id = "route-b".into();
        route_b.base_url = "https://route-b.example/v1".into();
        route_b.api_key = "route-b-secret".into();
        let applied = CodeyConfig {
            active_profile_id: "route-a".into(),
            profiles: vec![route_a, route_b],
            ..CodeyConfig::default()
        };
        let mut current = applied.clone();
        current.active_profile_id = "route-b".into();
        current.profiles[0].name = "Renamed Route A".into();
        current.default_model = "route-b/provider-default".into();
        current
            .selected_models_by_provider
            .insert("route-a".into(), vec!["route-a-model".into()]);

        assert!(!provider_route_requires_restart(&applied, &current));
        assert!(!websocket_transport_requires_restart(&applied, &current));
        assert!(runtime_supports_current_routes_for_hot_reload(
            &applied, &current
        ));
    }

    #[test]
    fn provider_route_restart_detection_catches_route_connection_changes() {
        let mut applied = CodeyConfig::default();
        applied.profiles[0].base_url = "https://route-a.example/v1".into();
        applied.profiles[0].api_key = "route-a-secret".into();
        let mut changed = applied.clone();
        changed.profiles[0].base_url = "https://route-a.example/v2".into();

        assert!(provider_route_requires_restart(&applied, &changed));
    }

    #[test]
    fn built_in_router_hot_reloads_added_and_removed_third_party_routes() {
        let mut route_a = crate::config::ProviderProfile::new("Route A");
        route_a.id = "route-a".into();
        route_a.base_url = "https://route-a.example/v1".into();
        route_a.api_key = "route-a-secret".into();
        let mut route_b = crate::config::ProviderProfile::new("Route B");
        route_b.id = "route-b".into();
        route_b.base_url = "https://route-b.example/v1".into();
        route_b.api_key = "route-b-secret".into();
        let applied = CodeyConfig {
            active_profile_id: "route-a".into(),
            profiles: vec![route_a.clone(), route_b],
            ..CodeyConfig::default()
        };

        let after_delete = CodeyConfig {
            active_profile_id: "route-a".into(),
            profiles: vec![route_a],
            ..applied.clone()
        };
        assert!(provider_route_requires_restart(&applied, &after_delete));
        assert!(runtime_supports_current_routes_for_hot_reload(
            &applied,
            &after_delete
        ));

        let mut route_c = crate::config::ProviderProfile::new("Route C");
        route_c.id = "route-c".into();
        route_c.base_url = "https://route-c.example/v1".into();
        route_c.api_key = "route-c-secret".into();
        let mut after_add = applied.clone();
        after_add.profiles.push(route_c);
        assert!(provider_route_requires_restart(&applied, &after_add));
        assert!(runtime_supports_current_routes_for_hot_reload(
            &applied, &after_add
        ));
    }

    #[test]
    fn websocket_model_changes_require_restart_and_stop_hot_reload() {
        let mut route = crate::config::ProviderProfile::new("WS Route");
        route.id = "route-ws".into();
        route.base_url = "https://route-ws.example/v1".into();
        route.api_key = "route-ws-secret".into();
        route.supports_websockets = true;
        route.normalize();
        let mut applied = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            ..CodeyConfig::default()
        };
        applied
            .selected_models_by_provider
            .insert("route-ws".into(), vec!["model-a".into()]);

        let mut after_add = applied.clone();
        after_add
            .selected_models_by_provider
            .get_mut("route-ws")
            .unwrap()
            .push("model-b".into());
        assert!(websocket_transport_requires_restart(&applied, &after_add));
        assert!(provider_route_requires_restart(&applied, &after_add));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &applied, &after_add
        ));

        let mut after_delete = applied.clone();
        after_delete
            .selected_models_by_provider
            .insert("route-ws".into(), vec!["model-b".into()]);
        assert!(websocket_transport_requires_restart(
            &applied,
            &after_delete
        ));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &applied,
            &after_delete
        ));
    }

    #[test]
    fn websocket_switch_changes_require_restart_and_stop_hot_reload() {
        let mut route = crate::config::ProviderProfile::new("Responses Route");
        route.id = "route-a".into();
        route.base_url = "https://route-a.example/v1".into();
        route.api_key = "route-a-secret".into();
        route.normalize();
        let mut applied = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            ..CodeyConfig::default()
        };
        applied
            .selected_models_by_provider
            .insert("route-a".into(), vec!["model-a".into()]);

        let mut enabled = applied.clone();
        enabled.profiles[0].supports_websockets = true;
        assert!(websocket_transport_requires_restart(&applied, &enabled));
        assert!(provider_route_requires_restart(&applied, &enabled));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &applied, &enabled
        ));

        assert!(websocket_transport_requires_restart(&enabled, &applied));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &enabled, &applied
        ));

        let mut mixed = enabled.clone();
        let mut http_route = crate::config::ProviderProfile::new("HTTP Route");
        http_route.id = "route-http".into();
        http_route.base_url = "https://route-http.example/v1".into();
        http_route.api_key = "route-http-secret".into();
        http_route.normalize();
        mixed.profiles.push(http_route);
        assert!(!websocket_transport_requires_restart(&enabled, &mixed));
        assert!(provider_route_requires_restart(&enabled, &mixed));
        assert!(runtime_supports_current_routes_for_hot_reload(
            &enabled, &mixed
        ));
    }

    #[test]
    fn native_web_search_switch_and_model_changes_require_restart() {
        let mut route = crate::config::ProviderProfile::new("Search Route");
        route.id = "route-search".into();
        route.base_url = "https://route-search.example/v1".into();
        route.api_key = "route-search-secret".into();
        route.normalize();
        let mut applied = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            ..CodeyConfig::default()
        };
        applied
            .selected_models_by_provider
            .insert("route-search".into(), vec!["model-a".into()]);

        let mut enabled = applied.clone();
        enabled.profiles[0].supports_native_web_search = true;
        assert!(native_web_search_capability_requires_restart(
            &applied, &enabled
        ));
        assert!(provider_route_requires_restart(&applied, &enabled));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &applied, &enabled
        ));

        let mut after_add = enabled.clone();
        after_add
            .selected_models_by_provider
            .get_mut("route-search")
            .unwrap()
            .push("model-b".into());
        assert!(native_web_search_capability_requires_restart(
            &enabled, &after_add
        ));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &enabled, &after_add
        ));
    }

    #[test]
    fn remote_compaction_identity_changes_require_restart_and_stop_hot_reload() {
        let mut route = crate::config::ProviderProfile::new("Responses Route");
        route.id = "route-a".into();
        route.base_url = "https://route-a.example/v1".into();
        route.api_key = "route-a-secret".into();
        route.normalize();
        let applied = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            ..CodeyConfig::default()
        };
        let mut enabled = applied.clone();
        enabled.profiles[0].supports_remote_compaction = true;

        assert!(remote_compaction_transport_requires_restart(
            &applied, &enabled
        ));
        assert!(provider_route_requires_restart(&applied, &enabled));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &applied, &enabled
        ));
        assert!(remote_compaction_transport_requires_restart(
            &enabled, &applied
        ));
    }

    #[test]
    fn official_websocket_transport_is_automatic_and_login_scoped() {
        let mut official = crate::config::ProviderProfile::new("OpenAI 官方直登");
        official.id = crate::config::DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();

        let mut available = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        }
        .normalize();
        available
            .selected_models_by_provider
            .insert("openai".into(), vec!["gpt-5.6-sol".into()]);

        assert!(available.runtime_supports_websockets());
        assert_eq!(
            available.runtime_websocket_model_aliases(),
            vec!["gpt-5.6-sol"]
        );

        let mut unavailable = available.clone();
        unavailable.official_account_available_this_launch = false;
        assert!(!unavailable.runtime_supports_websockets());
        assert!(unavailable.runtime_websocket_model_aliases().is_empty());
        assert!(websocket_transport_requires_restart(
            &available,
            &unavailable
        ));
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &available,
            &unavailable
        ));
    }
}
