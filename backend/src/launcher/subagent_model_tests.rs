use super::*;

fn subagent_catalog_fallback_config() -> CodeyConfig {
    let mut route = ProviderProfile::new("Relay");
    route.id = "route-a".into();
    route.base_url = "https://relay.example/v1".into();
    route.normalize();
    CodeyConfig {
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        local_router_enabled: true,
        subagent_optimization: true,
        selected_models_by_provider: std::collections::BTreeMap::from([(
            "route-a".into(),
            vec!["gpt-6-astra".into(), "vendor/model".into()],
        )]),
        subagent_model: "route-a/gpt-6-astra".into(),
        subagent_roles: crate::config::uniform_subagent_roles("route-a/gpt-6-astra", "high"),
        ..CodeyConfig::default()
    }
}

#[test]
fn subagent_catalog_fallback_uses_native_ids_without_mutating_saved_routes() {
    let mut config = subagent_catalog_fallback_config();
    config.subagent_roles.get_mut("codey_worker").unwrap().model = "route-a/vendor/model".into();
    config
        .subagent_roles
        .get_mut("codey_quick_scan")
        .unwrap()
        .reasoning_effort = "low".into();
    let saved = config.clone();
    let runtime = router_subagent_runtime_config(&config, false).unwrap();
    assert_eq!(runtime.subagent_model, "gpt-6-astra");
    assert_eq!(
        runtime.subagent_roles["codey_quick_scan"].model,
        "gpt-6-astra"
    );
    assert_eq!(
        runtime.subagent_roles["codey_quick_scan"].reasoning_effort,
        "low"
    );
    assert_eq!(runtime.subagent_roles["codey_worker"].model, "vendor/model");
    assert_eq!(config, saved);
    // A later settings save still uses the catalog mode of the running process.
    let reloaded = router_subagent_runtime_config(&config.normalize(), false).unwrap();
    assert_eq!(reloaded.subagent_roles, runtime.subagent_roles);
}

#[test]
fn subagent_catalog_fallback_rejects_ambiguous_routes_but_custom_catalog_preserves_them() {
    let mut config = subagent_catalog_fallback_config();
    let mut other = config.profiles[0].clone();
    other.id = "route-b".into();
    config.profiles.push(other);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec!["gpt-6-astra".into()]);
    config.subagent_roles.get_mut("codey_worker").unwrap().model = "route-b/gpt-6-astra".into();
    let error = router_subagent_runtime_config(&config, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("同名线路"), "{error}");
    let runtime = router_subagent_runtime_config(&config, true).unwrap();
    assert_eq!(
        runtime.subagent_roles["codey_worker"].model,
        "route-b/gpt-6-astra"
    );
    assert_eq!(
        runtime.subagent_roles["codey_quick_scan"].model,
        "route-a/gpt-6-astra"
    );
}

#[test]
fn subagent_catalog_fallback_checks_enabled_roles_only_and_rejects_missing_routes() {
    let mut config = subagent_catalog_fallback_config();
    let worker = config.subagent_roles.get_mut("codey_worker").unwrap();
    worker.model = "missing/model".into();
    assert!(router_subagent_runtime_config(&config, false).is_err());
    config
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .enabled = false;
    assert!(router_subagent_runtime_config(&config, false).is_ok());
    config.subagent_optimization = false;
    assert_eq!(
        router_subagent_runtime_config(&config, false).unwrap(),
        config
    );
}
