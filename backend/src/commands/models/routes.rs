use super::*;

pub async fn set_route_enabled(
    state: &Arc<AppState>,
    route_id: String,
    enabled: bool,
    expected_revision: u64,
) -> Result<Value, String> {
    let config_write_guard = state.config_write_lock.lock().await;
    let previous = state.config.read().await.clone();
    let mut config =
        config_after_route_enabled_change(&previous, route_id.trim(), enabled, expected_revision)?;
    let model_state = current_model_state_async(&config).await?;
    if config.subagent_optimization {
        reconcile_subagent_models_for_mode(&mut config, &model_state);
        config = config.normalize();
    }
    // 启停只保存线路及其模型依赖，不重新配置通知、日志保护等无关功能。
    let config = save_config_to_store(state, config).await?;
    *state.config.write().await = config.clone();
    drop(config_write_guard);
    let hot_reload = hot_reload_runtime_models(state, &config, &model_state).await;
    let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &config).await;
    let restart_required = runtime_config_requires_restart(state, &config).await;
    Ok(add_subagent_hot_reload_to_response(
        hot_reload.add_to_response(json!({
            "status": "ok",
            "config": redacted_config(&config),
            "providerStatus": codex_provider::status_from_config(&config),
            "modelState": model_state,
            "restartRequired": restart_required,
        })),
        subagent_hot_reload,
    ))
}

pub(crate) fn config_after_route_enabled_change(
    previous: &CodeyConfig,
    route_id: &str,
    enabled: bool,
    expected_revision: u64,
) -> Result<CodeyConfig, String> {
    ensure_local_route_config_writable(previous)?;
    ensure_route_revision(previous, expected_revision)?;
    let profile_index = previous
        .profiles
        .iter()
        .position(|profile| profile.id == route_id)
        .ok_or_else(|| "找不到要更新的线路".to_string())?;
    let profile = &previous.profiles[profile_index];
    if enabled && profile.official_account && !previous.official_route_usable(profile) {
        return Err("当前官方账号线路没有可用的登录态，无法启用".to_string());
    }
    let mut config = previous.clone();
    config.remember_model_aliases();
    config.profiles[profile_index].enabled = enabled;
    config = config.normalize();
    validate_provider_profiles(&config.profiles)?;
    config.settings_revision = previous.settings_revision.saturating_add(1);
    Ok(config)
}

pub async fn reorder_route_models(
    state: &Arc<AppState>,
    route_id: String,
    requested_models: Vec<String>,
    expected_revision: u64,
) -> Result<Value, String> {
    validate_requested_model_list_bounds("线路模型", &requested_models)?;
    let config_write_guard = state.config_write_lock.lock().await;
    let previous = state.config.read().await.clone();
    let config = config_with_reordered_route_models(
        &previous,
        route_id.trim(),
        &requested_models,
        expected_revision,
    )?;
    let model_state = current_model_state_async(&config).await?;
    // 只改模型顺序，成员、默认模型和子代理绑定都不变，直接落盘并推送目录。
    let config = save_config_to_store(state, config).await?;
    *state.config.write().await = config.clone();
    drop(config_write_guard);
    let hot_reload = hot_reload_runtime_models(state, &config, &model_state).await;
    let restart_required = runtime_config_requires_restart(state, &config).await;
    Ok(hot_reload.add_to_response(json!({
        "status": "ok",
        "config": redacted_config(&config),
        "modelState": model_state,
        "restartRequired": restart_required,
    })))
}

/// 线路内模型顺序以保存的启用列表为准。请求必须是当前启用模型的一个排列，
/// 不能借调整顺序增删模型。
pub(crate) fn config_with_reordered_route_models(
    previous: &CodeyConfig,
    route_id: &str,
    requested_models: &[String],
    expected_revision: u64,
) -> Result<CodeyConfig, String> {
    ensure_local_route_config_writable(previous)?;
    ensure_route_revision(previous, expected_revision)?;
    let profile = previous
        .profiles
        .iter()
        .find(|profile| profile.id == route_id)
        .ok_or_else(|| "找不到要调整模型顺序的线路".to_string())?;
    if profile.official_account && !previous.official_route_usable(profile) {
        return Err("当前线路不是本次登录可用的官方账号线路".to_string());
    }
    let provider_id = profile.provider_id().to_string();
    let current_models = if profile.official_account {
        previous.enabled_official_route_models(&provider_id)
    } else {
        previous.enabled_route_models(&provider_id)
    };
    let current_by_key = current_models
        .iter()
        .map(|model| (model_id::key(model), model.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut ordered = Vec::with_capacity(current_models.len());
    let mut seen = HashSet::new();
    for model in requested_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
    {
        let key = model_id::key(model);
        let Some(canonical) = current_by_key.get(key.as_str()) else {
            return Err(format!("模型 {model} 不属于该线路，无法调整顺序"));
        };
        if seen.insert(key) {
            ordered.push((*canonical).to_string());
        }
    }
    if ordered.len() != current_models.len() {
        return Err("线路模型列表已变化，请重新载入后再调整顺序".to_string());
    }
    let mut config = previous.clone();
    config
        .selected_models_by_provider
        .insert(provider_id, ordered);
    config = config.normalize();
    config.settings_revision = previous.settings_revision.saturating_add(1);
    Ok(config)
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
    let config = save_config_to_store(state, config).await?;
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
    // 每条官方线路都由官方账号列表派生，删除后仍会在下次派生时回来，因此统一
    // 引导用户去账号列表移除账号。
    if previous
        .profiles
        .iter()
        .any(|profile| profile.id == route_id && profile.official_account)
    {
        return Err("官方账号线路跟随账号列表自动生成，请在官方账号列表里移除该账号".to_string());
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
        .model_reasoning_efforts_by_provider
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

/// Account whose credentials should be used to list models for this official
/// route. Derived routes store the id on the profile; older routes only keep
/// it in the stable profile id. Plugin routes reuse the host login and have
/// neither.
pub(crate) fn official_models_account_id(profile: &ProviderProfile) -> Option<String> {
    let explicit = profile
        .official_account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if explicit.is_some() {
        return explicit;
    }
    let prefix = format!("{}-", crate::config::DERIVED_OFFICIAL_PROFILE_ID);
    profile
        .id
        .strip_prefix(&prefix)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Models are listed from the account gateway. A plugin route's own URL is the
/// Responses endpoint, so it must not be used as the models base.
pub(crate) fn official_models_base_url(
    profile: &ProviderProfile,
    account_base_url: Option<&str>,
) -> String {
    let account_base_url = account_base_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| url.trim_end_matches('/').to_string());
    if profile.plugin_owner_id.is_some() {
        return account_base_url
            .unwrap_or_else(|| crate::codex_config::CHATGPT_CODEX_BASE_URL.to_string());
    }
    account_base_url.unwrap_or_else(|| crate::codex_provider::official_route_base_url(profile))
}

async fn official_models_account(
    state: &Arc<AppState>,
    profile: &ProviderProfile,
) -> Result<crate::official_accounts::OfficialAccountRecord, String> {
    if let Some(account_id) = official_models_account_id(profile) {
        return super::super::official_accounts::refresh_official_account_tokens(
            state,
            &account_id,
        )
        .await;
    }
    // 插件官方线路和升级前的单条官方线路都不带账号标识，模型列表跟随宿主当前
    // 的默认官方账号；没有存储账号时再读 Codex 自己的登录文件。
    if let Some(account_id) = super::super::header_official_account_id(state).await {
        return super::super::official_accounts::refresh_official_account_tokens(
            state,
            &account_id,
        )
        .await;
    }
    let home = codex_home().to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::official_accounts::OfficialAccountStore::read_codex_login(&home)
    })
    .await
    .map_err(|error| format!("读取 Codex 登录信息任务异常退出：{error}"))?
    .map_err(|error| format!("{error:#}"))?
    .ok_or_else(|| "官方账号线路缺少账号标识".to_string())
}

pub(crate) async fn fetch_official_route_models(
    state: &Arc<AppState>,
    profile: &ProviderProfile,
) -> Result<Vec<Value>, String> {
    let record = official_models_account(state, profile).await?;
    let access_token = record
        .auth
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "官方账号登录信息缺少访问令牌".to_string())?;
    let base_url = official_models_base_url(profile, record.base_url.as_deref());
    let base_url = base_url.trim().trim_end_matches('/');
    let codex_app_path = state.config.read().await.codex_app_path.clone();
    let client_version = installed_codex_client_version(&codex_app_path);
    let endpoint = official_models_endpoint(base_url, client_version.as_deref());
    let client = if profile.upstream_proxy.trim().is_empty() {
        state.http_client.clone()
    } else {
        reqwest::Client::builder()
            .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(5))
            .proxy(
                reqwest::Proxy::all(profile.upstream_proxy.trim())
                    .map_err(|error| format!("官方线路的上游代理地址无效：{error}"))?,
            )
            .build()
            .map_err(|error| format!("创建官方模型请求客户端失败：{error}"))?
    };
    let mut request = client
        .get(endpoint)
        .timeout(std::time::Duration::from_secs(20))
        .header("accept", "application/json")
        .header("user-agent", "codex_cli_rs")
        .bearer_auth(access_token);
    if let Some(chatgpt_account_id) = record.account_id.as_deref() {
        request = request.header("chatgpt-account-id", chatgpt_account_id);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("请求官方模型列表失败：{error}"))?;
    let status = response.status();
    let body =
        crate::http_response::read_bounded_body(response, 4 * 1024 * 1024, "官方模型列表响应")
            .await
            .map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("官方模型列表接口返回 {status}"));
    }
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|error| format!("官方模型列表响应格式无效：{error}"))?;
    let models = ["models", "data", "items"]
        .iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_array))
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|model| {
            model
                .get("slug")
                .or_else(|| model.get("id"))
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty())
        })
        .map(|mut model| {
            if model.get("slug").and_then(Value::as_str).is_none()
                && let Some(id) = model.get("id").cloned()
            {
                model["slug"] = id;
            }
            model
        })
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("官方模型列表为空".to_string());
    }
    Ok(models)
}

const FALLBACK_OFFICIAL_MODELS_CLIENT_VERSION: &str = "0.154.0";

/// The official models endpoint omits slugs the reported client is too old to
/// use. The installed Codex version is what that build itself would send.
pub(crate) fn official_models_endpoint(base_url: &str, client_version: Option<&str>) -> String {
    let version = client_version
        .map(str::trim)
        .filter(|version| is_dotted_numeric_version(version))
        .unwrap_or(FALLBACK_OFFICIAL_MODELS_CLIENT_VERSION);
    format!("{base_url}/models?client_version={version}")
}

fn installed_codex_client_version(codex_app_path: &str) -> Option<String> {
    let configured = codex_app_path.trim();
    let app_dir = if configured.is_empty() {
        codey_runtime_core::app_paths::resolve_codex_app_dir(None)
    } else {
        codey_runtime_core::app_paths::resolve_codex_app_dir(Some(std::path::Path::new(configured)))
    }?;
    codey_runtime_core::app_paths::codex_app_version(&app_dir)
}

fn is_dotted_numeric_version(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

pub async fn fetch_route_models(
    state: &Arc<AppState>,
    route_id: String,
    expected_revision: u64,
) -> Result<Value, String> {
    let mut timings = ModelOperationTimings::new("fetch_route_models");
    let _provider_model_sync_guard = state.provider_model_sync_lock.lock().await;
    timings.mark("syncLockMs");
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
    profile.validate()?;
    let provider_id = profile.provider_id().to_string();
    let fetched_account_entries = if profile.official_account {
        Some(fetch_official_route_models(state, &profile).await?)
    } else {
        None
    };
    if let Some(entries) = fetched_account_entries.as_deref() {
        model_catalog::merge_account_runtime_models(codex_home(), entries)
            .map_err(|error| format!("保存官方模型目录快照失败：{error:#}"))?;
    }
    let fetched_models = if let Some(entries) = fetched_account_entries.as_ref() {
        entries
            .iter()
            .filter_map(|model| {
                model
                    .get("slug")
                    .or_else(|| model.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
    } else {
        fetch_provider_models(profile.clone())
            .await
            .map_err(|error| error.to_string())?
    };
    let visible_fetched_models = regular_route_models(fetched_models.clone());
    timings.mark("fetchModelsMs");
    let _config_write_guard = state.config_write_lock.lock().await;
    timings.mark("configLockMs");
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
    latest = if profile.official_account {
        let mut next = latest;
        next.upstream_models_by_provider
            .insert(provider_id.clone(), fetched_models.clone());
        let fallback_models = model_catalog::default_official_model_slugs();
        let selected = next.selected_models_by_provider.get(&provider_id);
        if selected.is_none_or(|models| models == &fallback_models) {
            next.selected_models_by_provider
                .insert(provider_id.clone(), fetched_models.clone());
        }
        next.normalize()
    } else {
        config_with_provider_model_sync(&latest, &provider_id, fetched_models.clone(), codex_home())
    };
    latest.settings_revision = latest.settings_revision.saturating_add(1);
    let RefreshedModelState {
        refresh: catalog_refresh,
        model_state,
        custom_contexts_restored,
    } = refreshed_model_state_with_context_recovery(
        &mut latest,
        true,
        crate::native_update_ui::confirm_context_recovery,
    )
    .await?;
    let route_model_state = if latest.active_profile_id == route_id {
        model_state.clone()
    } else {
        match model_state_for_route_async(&latest, route_id).await {
            Ok(route_state) => route_state,
            Err(error) => {
                return Err(
                    rollback_model_catalog_after_config_save_async(catalog_refresh, error).await,
                );
            }
        }
    };
    timings.mark("prepareModelsMs");
    let latest = match save_config_to_store(state, latest).await {
        Ok(latest) => latest,
        Err(error) => {
            return Err(
                rollback_model_catalog_after_config_save_async(catalog_refresh, error).await,
            );
        }
    };
    *state.config.write().await = latest.clone();
    drop(_config_write_guard);
    timings.mark("saveConfigMs");
    let hot_reload = hot_reload_runtime_models(state, &latest, &model_state).await;
    timings.mark("modelDeliveryMs");
    let subagent_hot_reload = hot_reload_runtime_subagent_config(state, &latest).await;
    timings.mark("subagentReloadMs");
    let restart_required = runtime_config_requires_restart(state, &latest).await;
    Ok(add_subagent_hot_reload_to_response(
        hot_reload.add_to_response(json!({
            "status":"ok",
            "config": redacted_config(&latest),
            "providerStatus": codex_provider::status_from_config(&latest),
            "models": visible_fetched_models,
            "modelState": model_state,
            "routeModelState": route_model_state,
            "customContextsRestored": custom_contexts_restored,
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
    next.retain_model_contexts(provider_id, &supported_models);
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

#[cfg(test)]
mod tests {
    use super::{official_models_account_id, official_models_base_url, official_models_endpoint};
    use crate::config::{AUTH_MODE_OFFICIAL_ACCOUNT, ProviderProfile, official_profile_id};

    #[test]
    fn official_models_account_id_uses_the_field_then_the_profile_id() {
        let mut profile = ProviderProfile::new("pro");
        profile.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        profile.id = official_profile_id("acct-from-id");
        profile.official_account_id = Some("acct-field".into());
        profile.normalize();
        assert_eq!(
            official_models_account_id(&profile).as_deref(),
            Some("acct-field")
        );

        profile.official_account_id = None;
        assert_eq!(
            official_models_account_id(&profile).as_deref(),
            Some("acct-from-id")
        );

        profile.id = "codey-official-account".into();
        assert_eq!(official_models_account_id(&profile), None);
    }

    #[test]
    fn plugin_official_route_lists_models_on_the_account_gateway() {
        let mut profile = ProviderProfile::new("Basis Points");
        profile.base_url = "https://bps.openai.com/basispoints/api/responses".into();
        profile.plugin_owner_id = Some("dev.codey.oai-basispoints".into());
        profile.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        profile.normalize();
        assert_eq!(
            official_models_base_url(&profile, None),
            "https://chatgpt.com/backend-api/codex"
        );
        assert_eq!(
            official_models_base_url(&profile, Some("https://gateway.example/codex/")),
            "https://gateway.example/codex"
        );
    }

    #[test]
    fn official_models_endpoint_uses_the_installed_client_version() {
        assert_eq!(
            official_models_endpoint(
                "https://chatgpt.com/backend-api/codex",
                Some("26.908.70816")
            ),
            "https://chatgpt.com/backend-api/codex/models?client_version=26.908.70816"
        );
        assert_eq!(
            official_models_endpoint(
                "https://chatgpt.com/backend-api/codex",
                Some("not-a-version")
            ),
            "https://chatgpt.com/backend-api/codex/models?client_version=0.154.0"
        );
        assert_eq!(
            official_models_endpoint("https://chatgpt.com/backend-api/codex", None),
            "https://chatgpt.com/backend-api/codex/models?client_version=0.154.0"
        );
    }
}
