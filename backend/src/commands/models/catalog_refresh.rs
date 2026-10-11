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

pub(crate) struct RefreshedModelState {
    pub(crate) refresh: Option<ModelCatalogRefresh>,
    pub(crate) model_state: model_catalog::ModelSelectionState,
    /// 保存时无法生成运行时目录，经用户确认后已清空自定义上下文预算。
    pub(crate) custom_contexts_restored: bool,
}

fn refresh_model_catalog_or_fallback_at(
    config: &CodeyConfig,
    home: &std::path::Path,
) -> Result<ModelCatalogRefresh, String> {
    crate::codex_config::validate_runtime_model_contexts(
        home,
        &config.runtime_enabled_model_contexts(),
    )
    .map_err(|error| error.to_string())?;
    let snapshot = model_catalog::snapshot(home).map_err(|error| error.to_string())?;
    let native_web_search_models = config.runtime_native_web_search_model_aliases();
    let image_detail_original_models = config.runtime_image_detail_original_model_aliases();
    let runtime_model_reasoning_efforts = config.runtime_model_reasoning_efforts();
    let runtime_model_contexts = config.runtime_model_contexts();
    let runtime_plugin_model_contexts = config.runtime_plugin_model_contexts();
    let runtime_display_names = config.runtime_catalog_display_names();
    let refresh = try_refresh_model_catalog(config, home);
    let reused_cached_catalog = refresh.is_err();
    let result = model_catalog_fallback(
        refresh,
        home,
        &native_web_search_models,
        &image_detail_original_models,
        &config.runtime_remote_compaction_model_aliases(),
    );
    match result {
        Ok(fallback) => {
            let available = model_catalog::is_available(home);
            if !runtime_plugin_model_contexts.is_empty() && !available {
                return Err(rollback_model_catalog_snapshot(
                    snapshot,
                    "插件模型需要可用的运行时模型目录，请恢复目录后重试".to_string(),
                ));
            }
            if !runtime_model_contexts.is_empty() && !available {
                return Err(rollback_model_catalog_snapshot(
                    snapshot,
                    model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE.to_string(),
                ));
            }
            // Freshly generated catalogs already include both overrides. Only
            // a reused catalog needs a separate, single read/write pass.
            if reused_cached_catalog
                && available
                && let Err(error) = model_catalog::apply_catalog_overrides(
                    home,
                    model_catalog::CatalogOverrides {
                        plugin_contexts: &runtime_plugin_model_contexts,
                        contexts: &runtime_model_contexts,
                        reasoning_efforts: &runtime_model_reasoning_efforts,
                        display_names: &runtime_display_names,
                    },
                )
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
    refreshed_model_state_at_async(config, codex_home(), refresh_only_when_populated).await
}

async fn refreshed_model_state_at_async(
    config: &CodeyConfig,
    home: &std::path::Path,
    refresh_only_when_populated: bool,
) -> Result<
    (
        Option<ModelCatalogRefresh>,
        model_catalog::ModelSelectionState,
    ),
    String,
> {
    let config = config.clone();
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if refresh_only_when_populated {
            let model_state = current_model_state_at(&config, &home)?;
            if !should_refresh_model_catalog(&model_state) {
                return Ok((None, model_state));
            }
        }
        let refresh = Some(refresh_model_catalog_or_fallback_at(&config, &home)?);
        match current_model_state_at(&config, &home) {
            Ok(model_state) => Ok((refresh, model_state)),
            Err(error) => Err(rollback_model_catalog_after_config_save(refresh, error)),
        }
    })
    .await
    .map_err(|error| format!("刷新 Codey 模型目录的任务异常退出：{error}"))?
}

/// 刷新运行时模型目录，并在自定义上下文预算无法生效时提供可恢复的处理。
///
/// 自定义预算要写入 Codex 配置，前提是本机能生成一份可用的运行时模型目录。
/// 本机模型缓存暂时不完整时，直接保存会让下次启动失败，所以这里先征询用户，
/// 同意后只清空自定义上下文预算并重新刷新，模型选择等改动照常保存。
/// 用户拒绝或对话框不可用时保持原样返回错误，绝不静默丢弃预算。
pub(crate) async fn refreshed_model_state_with_context_recovery<F, Fut>(
    config: &mut CodeyConfig,
    refresh_only_when_populated: bool,
    confirm: F,
) -> Result<RefreshedModelState, String>
where
    F: FnOnce(crate::native_update_ui::ContextRecoveryPurpose) -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>>,
{
    refreshed_model_state_with_context_recovery_at(
        config,
        codex_home(),
        refresh_only_when_populated,
        confirm,
    )
    .await
}

pub(crate) async fn refreshed_model_state_with_context_recovery_at<F, Fut>(
    config: &mut CodeyConfig,
    home: &std::path::Path,
    refresh_only_when_populated: bool,
    confirm: F,
) -> Result<RefreshedModelState, String>
where
    F: FnOnce(crate::native_update_ui::ContextRecoveryPurpose) -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>>,
{
    match refreshed_model_state_at_async(config, home, refresh_only_when_populated).await {
        Ok((refresh, model_state)) => Ok(RefreshedModelState {
            refresh,
            model_state,
            custom_contexts_restored: false,
        }),
        Err(error) if error == model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE => {
            let confirmed = confirm(crate::native_update_ui::ContextRecoveryPurpose::ModelSync)
                .await
                .unwrap_or(false);
            if !confirmed {
                return Err(error);
            }
            config.model_context_by_provider.clear();
            error_log::record_failure(
                "context_recovery",
                "restore_custom_context_budgets_for_model_save",
                error.clone(),
                serde_json::json!({"reason": "runtime_catalog_unavailable"}),
            );
            let (refresh, model_state) =
                refreshed_model_state_at_async(config, home, refresh_only_when_populated).await?;
            Ok(RefreshedModelState {
                refresh,
                model_state,
                custom_contexts_restored: true,
            })
        }
        Err(error) => Err(error),
    }
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
    if let Err(error) = save_config_to_store(state, persisted).await {
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
    image_detail_original_models: &[String],
    remote_compaction_models: &[String],
) -> Result<bool, String> {
    match result {
        Ok(()) => Ok(false),
        Err(error) if model_catalog::is_runtime_model_cache_unavailable(&error) => {
            model_catalog::prepare_cached_catalog_for_current_capabilities(
                home,
                native_web_search_models,
                image_detail_original_models,
                remote_compaction_models,
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
    let image_detail_original_models = config.runtime_image_detail_original_model_aliases();
    let remote_compaction_models = config.runtime_remote_compaction_model_aliases();
    model_catalog::refresh_for_provider_with_contexts(
        home,
        config.official_account_available_this_launch && use_builtin_official_catalog,
        (!use_builtin_official_catalog)
            .then_some(upstream_models)
            .as_deref(),
        &selected_models,
        model_catalog::CapabilityLists {
            websocket_models: Some(&websocket_models),
            native_web_search_models: Some(&native_web_search_models),
            image_detail_original_models: Some(&image_detail_original_models),
            remote_compaction_models: Some(&remote_compaction_models),
        },
        model_catalog::CatalogOverrides {
            plugin_contexts: &config.runtime_plugin_model_contexts(),
            contexts: &config.runtime_model_contexts(),
            reasoning_efforts: &config.runtime_model_reasoning_efforts(),
            display_names: &config.runtime_catalog_display_names(),
        },
        &config.codex_app_path,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_custom_context() -> CodeyConfig {
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
        config
    }

    #[test]
    fn custom_context_requires_a_runtime_catalog_before_save() {
        let home = tempfile::tempdir().unwrap();
        let config = config_with_custom_context();
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

    #[tokio::test]
    async fn custom_context_recovery_clears_budgets_after_confirmation() {
        let home = tempfile::tempdir().unwrap();
        let mut config = config_with_custom_context();
        let prompted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let prompted_by_confirm = Arc::clone(&prompted);

        let refreshed = refreshed_model_state_with_context_recovery_at(
            &mut config,
            home.path(),
            false,
            move |purpose| {
                prompted_by_confirm.store(
                    purpose == crate::native_update_ui::ContextRecoveryPurpose::ModelSync,
                    std::sync::atomic::Ordering::Relaxed,
                );
                async { Ok(true) }
            },
        )
        .await
        .unwrap();

        assert!(prompted.load(std::sync::atomic::Ordering::Relaxed));
        assert!(refreshed.custom_contexts_restored);
        assert!(config.model_context_by_provider.is_empty());
        assert!(config.runtime_model_contexts().is_empty());
        assert!(
            refreshed
                .refresh
                .as_ref()
                .is_some_and(|refresh| refresh.fallback)
        );
    }

    #[test]
    fn custom_context_save_validates_user_catalog_before_mutating_runtime_files() {
        let home = tempfile::tempdir().unwrap();
        let config = config_with_custom_context();
        std::fs::write(
            home.path().join("config.toml"),
            "model_catalog_json = 'custom.json'\n",
        )
        .unwrap();
        let source = home.path().join("custom.json");
        std::fs::write(&source, br#"{"models":[{"slug":"other-model"}]}"#).unwrap();
        let runtime = home.path().join(model_catalog::relative_path());
        std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
        std::fs::write(&runtime, b"previous catalog").unwrap();
        let error = refresh_model_catalog_or_fallback_at(&config, home.path())
            .err()
            .unwrap();
        assert!(error.contains("缺少已启用的预算模型"), "{error}");
        assert_eq!(std::fs::read(&runtime).unwrap(), b"previous catalog");
        assert!(
            !home
                .path()
                .join("model-catalogs/context-overrides")
                .exists()
        );
        let custom = br#"{"models":[{"slug":"gpt-5.6-sol","context_window":1000000}]}"#;
        std::fs::write(&source, custom).unwrap();
        std::fs::write(home.path().join("models_cache.json"), serde_json::to_vec(&json!({
            "models": [{"slug": "gpt-5.6-sol", "description": "Test model", "base_instructions": "Test instructions"}]
        })).unwrap()).unwrap();
        assert!(refresh_model_catalog_or_fallback_at(&config, home.path()).is_ok());
        assert_eq!(std::fs::read(&source).unwrap(), custom);
        assert!(
            !home
                .path()
                .join("model-catalogs/context-overrides")
                .exists()
        );
    }

    #[test]
    fn custom_context_overlay_only_requires_enabled_models() {
        let mut config = config_with_custom_context();
        let policy = config.model_context_by_provider["openai"]["gpt-5.6-sol"].clone();
        config
            .model_context_by_provider
            .get_mut("openai")
            .unwrap()
            .insert("gpt-5.5".into(), policy);
        assert_eq!(config.runtime_model_contexts().len(), 2);
        assert_eq!(
            config
                .runtime_enabled_model_contexts()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["gpt-5.6-sol"]
        );
        config.profiles[0].enabled = false;
        assert!(config.runtime_enabled_model_contexts().is_empty());
        config.profiles[0].enabled = true;
        config.local_router_enabled = false;
        assert!(config.runtime_enabled_model_contexts().is_empty());
    }

    #[tokio::test]
    async fn custom_context_recovery_keeps_budgets_when_declined() {
        let home = tempfile::tempdir().unwrap();
        let mut config = config_with_custom_context();
        let original = config.model_context_by_provider.clone();

        let error = refreshed_model_state_with_context_recovery_at(
            &mut config,
            home.path(),
            false,
            |_| async { Ok(false) },
        )
        .await
        .err()
        .unwrap();

        assert_eq!(error, model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE);
        assert_eq!(config.model_context_by_provider, original);
        assert!(!home.path().join(model_catalog::relative_path()).exists());
    }

    #[tokio::test]
    async fn custom_context_recovery_fails_closed_without_a_prompt() {
        let home = tempfile::tempdir().unwrap();
        let mut config = config_with_custom_context();

        let error = refreshed_model_state_with_context_recovery_at(
            &mut config,
            home.path(),
            false,
            |_| async { Err("对话框不可用".to_string()) },
        )
        .await
        .err()
        .unwrap();

        assert_eq!(error, model_catalog::CUSTOM_CONTEXT_CATALOG_UNAVAILABLE);
        assert!(!config.model_context_by_provider.is_empty());
    }
}
