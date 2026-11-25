use super::*;
use crate::config::ModelContextConfig;
use std::path::Path;

const MODEL: &str = "gpt-6-luna";
const PROVIDER: &str = "context-test";
const ALIAS: &str = "context-test/gpt-6-luna";

fn config_with_budget(window: Option<u64>) -> CodeyConfig {
    let mut profile = ProviderProfile::new("Relay");
    profile.id = PROVIDER.into();
    profile.base_url = "https://relay.example/v1".into();
    profile.api_key = "test-key".into();
    profile.normalize();
    let mut config = CodeyConfig {
        local_router_enabled: true,
        active_profile_id: profile.id.clone(),
        selected_models_by_provider: BTreeMap::from([(PROVIDER.into(), vec![MODEL.into()])]),
        profiles: vec![profile],
        ..CodeyConfig::default()
    }
    .normalize();
    if let Some(window) = window {
        config.model_context_by_provider.insert(
            PROVIDER.into(),
            BTreeMap::from([(
                MODEL.into(),
                ModelContextConfig {
                    context_window_tokens: window,
                    auto_compact_token_limit: None,
                    reserve_output_tokens: None,
                },
            )]),
        );
    }
    config
}

fn save_catalog(home: &Path, config: &CodeyConfig) -> Vec<u8> {
    let base = json!({
        "context_window": 272000,
        "max_context_window": 872000,
        "effective_context_window_percent": 95,
        "auto_compact_token_limit": null,
        "codey_context_source": "official_catalog"
    });
    let mut model = base.clone();
    model["slug"] = json!(ALIAS);
    model["description"] = json!("Test model");
    model["base_instructions"] = json!("Test instructions");
    model["codey_context_base"] = base;
    if let Some(policy) = config.runtime_plugin_model_contexts().get(ALIAS) {
        model_catalog::apply_model_context(&mut model, Some(policy)).unwrap();
        model["codey_context_source"] = json!("plugin_declared");
    }
    model_catalog::apply_model_context(&mut model, config.model_context(PROVIDER, MODEL)).unwrap();
    let path = home.join(model_catalog::relative_path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = serde_json::to_vec(&json!({"models": [model]})).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn renderer_budget(home: &Path, config: &CodeyConfig) -> Value {
    let model_state = model_catalog::ModelSelectionState {
        third_party_models: vec![MODEL.into()],
        ..Default::default()
    };
    let catalog = renderer_model_catalog_value_at(config, &model_state, home).unwrap();
    let metadata = catalog["model_metadata"].as_array().unwrap();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0]["model"], ALIAS);
    metadata[0].clone()
}

#[test]
fn renderer_context_budget_waits_for_restart_after_shared_catalog_changes() {
    for (old, new, expected_window, expected_threshold) in [
        (None, Some(400_000), 272_000, None),
        (Some(128_000), Some(400_000), 128_000, Some(115_200)),
        (Some(400_000), None, 400_000, Some(360_000)),
    ] {
        let home = tempfile::tempdir().unwrap();
        let applied = config_with_budget(old);
        let saved = config_with_budget(new);
        let bytes = save_catalog(home.path(), &saved);
        let delivered = config_with_launch_pinned_transport(&applied, &saved);
        assert!(!runtime_supports_current_routes_for_hot_reload(
            &applied, &saved
        ));

        // 热更新和页面重连均使用已生效配置，不能读取共享目录里的待重启预算。
        for config in [&delivered, &applied] {
            let metadata = renderer_budget(home.path(), config);
            assert_eq!(metadata["context_window"], expected_window);
            assert_eq!(
                metadata["auto_compact_token_limit"],
                json!(expected_threshold)
            );
        }
        let restarted = renderer_budget(home.path(), &saved);
        assert_eq!(restarted["context_window"], new.unwrap_or(272_000));
        assert_eq!(
            restarted["auto_compact_token_limit"],
            json!(new.map(|window| window * 9 / 10))
        );
        assert_eq!(
            std::fs::read(home.path().join(model_catalog::relative_path())).unwrap(),
            bytes,
            "读取已生效预算不能改写为下次启动准备的目录"
        );
    }
}

#[test]
fn renderer_context_budget_retains_plugin_default_until_restart() {
    let home = tempfile::tempdir().unwrap();
    let mut applied = config_with_budget(None);
    applied.profiles[0].plugin_owner_id = Some("dev.context-test".into());
    applied.profiles[0].plugin_route_spec = Some(
        serde_json::from_value(json!({
            "name": "Test plugin", "baseUrl": "https://relay.example/v1",
            "upstreamProtocol": "openaiResponses", "models": [MODEL], "headers": {},
            "transport": {"accountEmail": "test@example.com", "models": {MODEL: {
                "contextWindow": 300000, "autoCompactTokenLimit": 250000
            }}}
        }))
        .unwrap(),
    );
    let mut saved = applied.clone();
    saved.model_context_by_provider = config_with_budget(Some(400_000)).model_context_by_provider;
    save_catalog(home.path(), &saved);
    let delivered = config_with_launch_pinned_transport(&applied, &saved);
    let metadata = renderer_budget(home.path(), &delivered);
    assert_eq!(metadata["context_window"], 300_000);
    assert_eq!(metadata["auto_compact_token_limit"], 250_000);
    assert_eq!(metadata["codey_context_source"], "plugin_declared");
    let restarted = renderer_budget(home.path(), &saved);
    assert_eq!(restarted["context_window"], 400_000);
    assert_eq!(restarted["auto_compact_token_limit"], 360_000);
}

fn official_budget_config(models: &[&str]) -> CodeyConfig {
    let mut profile = ProviderProfile::new("Official");
    profile.source_provider_id = Some("openai".into());
    profile.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
    profile.normalize();
    let selected = BTreeMap::from([(
        "openai".into(),
        models.iter().map(|model| model.to_string()).collect(),
    )]);
    CodeyConfig {
        local_router_enabled: true,
        active_profile_id: profile.id.clone(),
        profiles: vec![profile],
        official_account_available_this_launch: true,
        selected_models_by_provider: selected.clone(),
        upstream_models_by_provider: selected,
        ..Default::default()
    }
    .normalize()
}

fn official_budget_catalog(home: &Path, window: u64, limit: u64) -> std::path::PathBuf {
    let path = home.join(model_catalog::relative_path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let models = ["gpt-5.6-sol", "gpt-5.6-luna"].map(|model| {
        json!({
            "slug": model, "description": "Official", "base_instructions": "Instructions",
            "context_window": window, "max_context_window": 872000,
            "auto_compact_token_limit": limit, "effective_context_window_percent": 95,
            "codey_context_source": "official_catalog",
        })
    });
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"models": models})).unwrap(),
    )
    .unwrap();
    path
}

fn official_renderer_catalog(home: &Path, config: &CodeyConfig) -> Value {
    let model_state = model_catalog::ModelSelectionState {
        official_models: config
            .enabled_official_route_models("openai")
            .into_iter()
            .map(|slug| model_catalog::OfficialModelAvailability {
                display_name: slug.clone(),
                slug,
                supported: true,
                supported_reasoning_efforts: vec!["low".into()],
                default_reasoning_effort: "low".into(),
            })
            .collect(),
        ..Default::default()
    };
    renderer_model_catalog_value_at(config, &model_state, home).unwrap()
}

#[test]
fn official_catalog_budget_changes_stay_pending_until_a_new_launch() {
    use crate::launcher::CodeyRuntime;
    for (old_window, old_limit) in [(272_000, 240_000), (272_000, 244_800), (400_000, 240_000)] {
        let home = tempfile::tempdir().unwrap();
        let config = official_budget_config(&["gpt-5.6-sol"]);
        let path = official_budget_catalog(home.path(), old_window, old_limit);
        let runtime = CodeyRuntime::for_model_delivery_test(config.clone(), "", None)
            .with_context_budgets_for_test(home.path(), Some(&path));
        official_budget_catalog(home.path(), 400_000, 360_000);
        // 正向应用读取共享目录，重复同步不能将启动基线更新为待重启预算。
        for _ in 0..2 {
            let mut catalog = official_renderer_catalog(home.path(), &config);
            assert_eq!(catalog["model_metadata"][0]["context_window"], 400_000);
            runtime.apply_renderer_context_budgets(&mut catalog);
            assert!(runtime.context_budget_requires_restart());
            assert_eq!(catalog["model_metadata"][0]["context_window"], old_window);
            assert_eq!(
                catalog["model_metadata"][0]["auto_compact_token_limit"],
                old_limit
            );
        }
        let mut reconnected = official_renderer_catalog(home.path(), &config);
        runtime.pin_renderer_context_budgets(&mut reconnected);
        assert_eq!(
            reconnected["model_metadata"][0]["auto_compact_token_limit"],
            old_limit
        );
        // 状态查询完全使用内存，即使目录此时无法读取，也不会丢失重启标记。
        std::fs::remove_file(&path).unwrap();
        for _ in 0..3 {
            assert!(runtime.context_budget_requires_restart());
        }
        official_budget_catalog(home.path(), 400_000, 360_000);
        let restarted = CodeyRuntime::for_model_delivery_test(config.clone(), "", None)
            .with_context_budgets_for_test(home.path(), Some(&path));
        let mut catalog = official_renderer_catalog(home.path(), &config);
        restarted.apply_renderer_context_budgets(&mut catalog);
        assert!(!restarted.context_budget_requires_restart());
        assert_eq!(catalog["model_metadata"][0]["context_window"], 400_000);
        assert_eq!(
            catalog["model_metadata"][0]["auto_compact_token_limit"],
            360_000
        );
    }
}

#[test]
fn official_catalog_membership_changes_only_compare_running_models() {
    use crate::launcher::CodeyRuntime;
    let home = tempfile::tempdir().unwrap();
    let applied = official_budget_config(&["gpt-5.6-sol"]);
    let path = official_budget_catalog(home.path(), 400_000, 360_000);
    let runtime = CodeyRuntime::for_model_delivery_test(applied.clone(), "", None)
        .with_context_budgets_for_test(home.path(), Some(&path));
    for models in [vec!["gpt-5.6-sol", "gpt-5.6-luna"], vec!["gpt-5.6-luna"]] {
        let current = official_budget_config(&models);
        let mut catalog = official_renderer_catalog(home.path(), &current);
        for entry in catalog["model_metadata"].as_array_mut().unwrap() {
            if entry["model"] == "gpt-5.6-luna" {
                entry["auto_compact_token_limit"] = json!(300_000);
            }
        }
        runtime.apply_renderer_context_budgets(&mut catalog);
        assert!(!runtime.context_budget_requires_restart());
    }
    runtime.apply_renderer_context_budgets(&mut json!({"model_metadata": []}));
    assert!(!runtime.context_budget_requires_restart());
}

#[test]
fn official_builtin_budget_is_preserved_when_sync_installs_a_codey_default() {
    use crate::launcher::CodeyRuntime;
    let home = tempfile::tempdir().unwrap();
    let config = official_budget_config(&["gpt-5.6-sol"]);
    let path = official_budget_catalog(home.path(), 272_000, 240_000);
    model_catalog::write_test_runtime_catalog(
        home.path(),
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap(),
        true,
    );
    let runtime = CodeyRuntime::for_model_delivery_test(config.clone(), "", None)
        .with_context_budgets_for_test(home.path(), None);
    official_budget_catalog(home.path(), 400_000, 360_000);
    let mut catalog = official_renderer_catalog(home.path(), &config);
    runtime.apply_renderer_context_budgets(&mut catalog);
    assert!(runtime.context_budget_requires_restart());
    assert_eq!(catalog["model_metadata"][0]["context_window"], 272_000);
    assert_eq!(
        catalog["model_metadata"][0]["auto_compact_token_limit"],
        240_000
    );
}

#[test]
fn official_budget_reduction_requires_restart_before_leaving_the_codey_catalog() {
    use crate::launcher::CodeyRuntime;
    let home = tempfile::tempdir().unwrap();
    let config = official_budget_config(&["gpt-5.6-sol"]);
    let path = official_budget_catalog(home.path(), 400_000, 360_000);
    let runtime = CodeyRuntime::for_model_delivery_test(config.clone(), "", None)
        .with_context_budgets_for_test(home.path(), Some(&path));
    official_budget_catalog(home.path(), 272_000, 240_000);
    let mut reduced: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    reduced["models"][0]["slug"] = json!("GPT-5.6-Sol");
    for model in reduced["models"].as_array_mut().unwrap() {
        model["max_context_window"] = json!(272_000);
    }
    std::fs::write(&path, serde_json::to_vec(&reduced).unwrap()).unwrap();
    let mut catalog = official_renderer_catalog(home.path(), &config);
    assert_eq!(catalog["model_metadata"][0]["context_window"], 272_000);
    runtime.apply_renderer_context_budgets(&mut catalog);
    assert!(runtime.context_budget_requires_restart());
    assert_eq!(catalog["model_metadata"][0]["context_window"], 400_000);
}

#[test]
fn custom_catalog_only_monitors_models_filled_from_shared_catalog() {
    use crate::launcher::CodeyRuntime;
    let home = tempfile::tempdir().unwrap();
    let config = official_budget_config(&["gpt-5.6-sol", "gpt-5.6-luna"]);
    official_budget_catalog(home.path(), 272_000, 240_000);
    let custom = home.path().join("custom.json");
    // 预算快照只读取预算，不要求自备目录采用 Codey 的指令模板格式。
    let original = serde_json::to_vec(&json!({"models": [{
        "slug": "gpt-5.6-luna", "context_window": 128000,
        "auto_compact_token_limit": 100000, "codey_context_source": "official_catalog"
    }]}))
    .unwrap();
    std::fs::write(&custom, &original).unwrap();
    let overlay = model_catalog::prepare_context_catalog_overlay(
        home.path(),
        &custom,
        &BTreeMap::new(),
        &model_catalog::RequiredCatalogModels {
            models: vec!["gpt-5.6-sol".into(), "gpt-5.6-luna".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_ne!(overlay, custom);
    let runtime = CodeyRuntime::for_model_delivery_test(config.clone(), "", None)
        .with_context_budgets_for_test(home.path(), Some(&overlay));
    official_budget_catalog(home.path(), 400_000, 360_000);
    let mut catalog = official_renderer_catalog(home.path(), &config);
    runtime.apply_renderer_context_budgets(&mut catalog);
    assert!(runtime.context_budget_requires_restart());
    let entries = catalog["model_metadata"].as_array().unwrap();
    assert_eq!(entries[0]["context_window"], 272_000);
    assert_eq!(entries[1]["context_window"], 128_000);
    let own_only = official_budget_config(&["gpt-5.6-luna"]);
    runtime.apply_renderer_context_budgets(&mut official_renderer_catalog(home.path(), &own_only));
    assert!(!runtime.context_budget_requires_restart());
    assert_eq!(std::fs::read(&custom).unwrap(), original);
    // 仅使用自备条目时，共享目录不存在也不应增加启动条件。
    std::fs::remove_file(home.path().join(model_catalog::relative_path())).unwrap();
    let own_runtime = CodeyRuntime::for_model_delivery_test(own_only, "", None)
        .with_context_budgets_for_test(home.path(), Some(&custom));
    assert!(!own_runtime.context_budget_requires_restart());
}

#[test]
fn official_catalog_sync_respects_active_global_budget_fields() {
    let home = tempfile::tempdir().unwrap();
    let config = official_budget_config(&["gpt-5.6-sol"]);
    for (settings, applied_window, needs_restart) in [
        (
            "model_context_window=128000\nmodel_auto_compact_token_limit=100000",
            128_000,
            false,
        ),
        ("model_auto_compact_token_limit=100000", 272_000, true),
    ] {
        let profile = format!("profile='work'\n[profiles.work]\n{settings}");
        let global =
            crate::codex_config::GlobalContextBudget::from_document(&profile.parse().unwrap());
        let path = official_budget_catalog(home.path(), 272_000, 240_000);
        let launch = model_catalog::RuntimeContextBudgets::from_catalog(
            home.path(),
            &config,
            Some(&path),
            global,
        )
        .unwrap();
        official_budget_catalog(home.path(), 400_000, 360_000);
        let mut catalog = official_renderer_catalog(home.path(), &config);
        assert_eq!(launch.pin_renderer_catalog(&mut catalog), needs_restart);
        let current = &catalog["model_metadata"][0];
        assert_eq!(current["context_window"], applied_window);
        assert_eq!(current["auto_compact_token_limit"], 100_000);
    }
}

#[test]
fn new_route_does_not_inherit_another_routes_launch_budget() {
    use crate::launcher::CodeyRuntime;
    let home = tempfile::tempdir().unwrap();
    let config = official_budget_config(&["gpt-5.6-sol"]);
    let path = official_budget_catalog(home.path(), 272_000, 240_000);
    let runtime = CodeyRuntime::for_model_delivery_test(config.clone(), "", None)
        .with_context_budgets_for_test(home.path(), Some(&path));
    let mut catalog = official_renderer_catalog(home.path(), &config);
    catalog["model_metadata"][0]["route_provider_id"] = json!("new-official-route");
    catalog["model_metadata"][0]["context_window"] = json!(400_000);
    runtime.apply_renderer_context_budgets(&mut catalog);
    assert!(!runtime.context_budget_requires_restart());
    assert_eq!(catalog["model_metadata"][0]["context_window"], 400_000);
}

#[test]
fn unreadable_context_catalog_is_reported_instead_of_becoming_an_empty_budget() {
    let home = tempfile::tempdir().unwrap();
    let config = official_budget_config(&["gpt-5.6-sol"]);
    let path = official_budget_catalog(home.path(), 400_000, 360_000);
    std::fs::write(path, b"invalid json").unwrap();
    let error =
        renderer_model_catalog_value_at(&config, &Default::default(), home.path()).unwrap_err();
    assert!(error.contains("解析 Codey 运行时模型目录失败"));
}
