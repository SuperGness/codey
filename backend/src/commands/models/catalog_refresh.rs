use super::*;

pub(crate) fn should_refresh_model_catalog(
    model_state: &model_catalog::ModelSelectionState,
) -> bool {
    !model_state.official_models.is_empty() || !model_state.third_party_models.is_empty()
}

pub(crate) struct ModelCatalogRefresh {
    pub(crate) fallback: bool,
    pub(crate) snapshot: model_catalog::CatalogSnapshot,
}

pub(crate) fn refresh_model_catalog_or_fallback(
    config: &CodeyConfig,
) -> Result<ModelCatalogRefresh, String> {
    refresh_model_catalog_or_fallback_at(config, codex_home())
}

fn refresh_model_catalog_or_fallback_at(
    config: &CodeyConfig,
    home: &std::path::Path,
) -> Result<ModelCatalogRefresh, String> {
    let snapshot = model_catalog::snapshot(home).map_err(|error| error.to_string())?;
    let native_web_search_models = config.runtime_native_web_search_model_aliases();
    let context_1m_models = config.runtime_1m_context_model_aliases();
    let result = model_catalog_fallback(
        try_refresh_model_catalog(config, home),
        home,
        &native_web_search_models,
        &context_1m_models,
    );
    match result {
        Ok(fallback) => {
            if !config.runtime_model_contexts().is_empty() && !model_catalog::is_available(home) {
                return Err(rollback_model_catalog_snapshot(
                    snapshot,
                    model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE.to_string(),
                ));
            }
            if model_catalog::is_available(home)
                && let Err(error) =
                    model_catalog::apply_catalog_contexts(home, &config.runtime_model_contexts())
            {
                return Err(rollback_model_catalog_snapshot(snapshot, error.to_string()));
            }
            Ok(ModelCatalogRefresh { fallback, snapshot })
        }
        Err(error) => Err(rollback_model_catalog_snapshot(snapshot, error)),
    }
}

pub(crate) async fn refreshed_model_state_async(
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

pub(crate) async fn reconcile_current_subagent_defaults(
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

pub(crate) fn config_with_reconciled_subagent_defaults(
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

pub(crate) fn rollback_model_catalog_after_config_save(
    refresh: Option<ModelCatalogRefresh>,
    error: String,
) -> String {
    match refresh {
        Some(refresh) => rollback_model_catalog_snapshot(refresh.snapshot, error),
        None => error,
    }
}

pub(crate) async fn rollback_model_catalog_after_config_save_async(
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

pub(crate) fn rollback_model_catalog_snapshot(
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

pub(crate) fn model_catalog_fallback(
    result: anyhow::Result<()>,
    home: &std::path::Path,
    native_web_search_models: &[String],
    context_1m_models: &[String],
) -> Result<bool, String> {
    match result {
        Ok(()) => Ok(false),
        Err(error) if model_catalog::is_runtime_model_cache_unavailable(&error) => {
            model_catalog::prepare_cached_catalog_for_current_capabilities(
                home,
                native_web_search_models,
                context_1m_models,
            )
            .map(|available| !available)
            .map_err(|fallback_error| fallback_error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn try_refresh_model_catalog(config: &CodeyConfig, home: &std::path::Path) -> anyhow::Result<()> {
    let use_builtin_official_catalog = config.uses_builtin_official_model_catalog();
    let (upstream_models, selected_models) = config.runtime_catalog_models();
    let websocket_models = config.runtime_websocket_model_aliases();
    let native_web_search_models = config.runtime_native_web_search_model_aliases();
    let context_1m_models = config.runtime_1m_context_model_aliases();
    model_catalog::refresh_for_provider_with_capabilities(
        home,
        config.official_account_available_this_launch && use_builtin_official_catalog,
        (!use_builtin_official_catalog)
            .then_some(upstream_models)
            .as_deref(),
        &selected_models,
        &websocket_models,
        &native_web_search_models,
        &context_1m_models,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_context_requires_a_runtime_catalog_before_save() {
        let home = tempfile::tempdir().unwrap();
        let mut official = crate::config::ProviderProfile::new("Official");
        official.source_provider_id = Some("openai".into());
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        let mut config = CodeyConfig {
            local_router_enabled: true,
            active_profile_id: official.id.clone(),
            profiles: vec![official],
            official_account_available_this_launch: true,
            selected_models_by_provider: BTreeMap::from([(
                "openai".into(),
                vec!["gpt-5.6-sol".into()],
            )]),
            ..CodeyConfig::default()
        }
        .normalize();
        let policy = crate::config::ModelContextConfig {
            context_window_tokens: 256_000,
            auto_compact_token_limit: None,
            reserve_output_tokens: None,
        };
        config.model_context_by_provider.insert(
            "openai".into(),
            BTreeMap::from([("gpt-5.6-sol".into(), policy)]),
        );
        assert!(!config.runtime_model_contexts().is_empty());
        let result = refresh_model_catalog_or_fallback_at(&config, home.path());
        assert_eq!(
            result.err().unwrap(),
            model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE
        );
        assert!(!home.path().join(model_catalog::relative_path()).exists());

        std::fs::write(home.path().join("models_cache.json"), serde_json::to_vec(&json!({
            "models": [{"slug": "gpt-5.6-sol", "description": "Test model", "base_instructions": "Test instructions"}]
        })).unwrap()).unwrap();
        assert!(refresh_model_catalog_or_fallback_at(&config, home.path()).is_ok());
        let catalog: Value = serde_json::from_slice(
            &std::fs::read(home.path().join(model_catalog::relative_path())).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog["models"][0]["context_window"], 256_000);
        assert_eq!(catalog["models"][0]["auto_compact_token_limit"], 230_400);
    }
}
