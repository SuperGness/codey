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
    let catalog = renderer_model_catalog_value_at(config, &model_state, home);
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
