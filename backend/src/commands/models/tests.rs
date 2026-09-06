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
        profiles: vec![route],
        upstream_models_by_provider: BTreeMap::from([(
            "route-a".into(),
            vec!["model-a".into(), "model-b".into()],
        )]),
        subagent_model: "route-a/model-a".into(),
        subagent_roles: crate::config::uniform_subagent_roles("route-a/model-a", "high"),
        ..CodeyConfig::default()
    }
    .normalize();

    let mut selected =
        config_with_native_selected_models(&config, &provider, &[], &["model-b".into()], &[], &[])
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
        &["gpt-5.6-sol".into()],
        &[],
        &[],
        &[],
    )
    .unwrap();

    assert_eq!(
        selected.selected_models_by_provider["openai"],
        ["gpt-5.6-sol"]
    );
    let home = tempfile::tempdir().unwrap();
    let model_state = native_model_state_for_provider(&selected, &provider, home.path()).unwrap();
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
            &["gpt-5.6-sol".into()],
            &["custom-model".into()],
            &[],
            &[],
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
    let selected =
        config_with_native_selected_models(&config, &provider, &[], &["gpt-5.5".into()], &[], &[])
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
fn deleted_route_aliases_survive_native_catalog_and_model_conversion() {
    let config = CodeyConfig {
        local_router_enabled: false,
        model_alias_history: BTreeMap::from([(
            "deleted/vendor/model".into(),
            "vendor/model".into(),
        )]),
        selected_models_by_provider: BTreeMap::from([(
            "native".into(),
            vec!["codey/raw-model".into()],
        )]),
        ..CodeyConfig::default()
    };
    for (input, expected) in [
        ("deleted/vendor/model", "vendor/model"),
        ("CODEY/vendor/model", "CODEY/vendor/model"),
        ("codey/raw-model", "codey/raw-model"),
        ("vendor/model", "vendor/model"),
        ("unknown/vendor/model", "unknown/vendor/model"),
    ] {
        assert_eq!(native_upstream_model(&config, input), expected);
    }
    let catalog =
        renderer_model_catalog_value(&config, &model_catalog::ModelSelectionState::default());
    assert_eq!(
        catalog["legacy_model_aliases"]["deleted/vendor/model"],
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
        selected_models_by_provider: BTreeMap::from([("route-b".into(), vec!["model-b".into()])]),
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
