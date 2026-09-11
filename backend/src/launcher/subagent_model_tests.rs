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

#[test]
fn subagent_catalog_fallback_disables_only_this_launch_on_invalid_routes() {
    let mut config = subagent_catalog_fallback_config();
    let saved = config.clone();
    let roles = startup_router_subagent_runtime_config(&mut config, false);
    assert!(roles.subagent_optimization);
    assert_eq!(roles.subagent_model, "gpt-6-astra");
    assert_eq!(config, saved);

    let mut other = config.profiles[0].clone();
    other.id = "route-b".into();
    config.profiles.push(other);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec!["gpt-6-astra".into()]);
    let mut missing = saved;
    missing
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .model = "missing/model".into();
    let mut restored = config.clone();
    let roles = startup_router_subagent_runtime_config(&mut restored, true);
    assert!(roles.subagent_optimization);
    assert_eq!(roles.subagent_model, "route-a/gpt-6-astra");
    assert_eq!(restored, config);
    for saved in [config, missing] {
        let mut runtime = saved.clone();
        let roles = startup_router_subagent_runtime_config(&mut runtime, false);
        assert!(!runtime.subagent_optimization);
        assert_eq!(roles, runtime);
        runtime.subagent_optimization = true;
        assert_eq!(runtime, saved);
        // A failed startup check must not weaken validation during hot reload.
        assert!(router_subagent_runtime_config(&saved, false).is_err());
    }
}

#[tokio::test]
async fn subagent_catalog_fallback_keeps_live_routes_and_roles_until_restart() {
    let mut config = subagent_catalog_fallback_config();
    let mut other = config.profiles[0].clone();
    other.id = "route-b".into();
    config.profiles.push(other);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec!["other-model".into()]);
    let router = LocalRouter::start(&config).await.unwrap();
    let snapshot = Arc::clone(&router.snapshot);
    let mut runtime = CodeyRuntime {
        codex_app_path: PathBuf::new(),
        maintenance: MaintenanceStatus {
            session_status: String::new(),
            session_files_fixed: 0,
            sqlite_rows_updated: 0,
            ghost_tasks_pruned: 0,
            performance_status: String::new(),
            performance_detail: String::new(),
            startup_injection_mode: String::new(),
        },
        applied_model_config: RwLock::new(RuntimeModelConfig::from_config(&config)),
        applied_subagent_config: RwLock::new(RuntimeSubagentConfig::from_config(&config)),
        applied_config: config.clone(),
        subagent_route_catalog_installed: false,
        injection_statuses: Arc::new(RwLock::new(Arc::from([]))),
        injection_scripts: cdp::prepare_injection_scripts(false, false, false, &[]),
        injection_websocket_url: Arc::new(RwLock::new(Arc::from(""))),
        child: Arc::new(Mutex::new(None)),
        process_id: None,
        #[cfg(unix)]
        process_group_id: None,
        #[cfg(target_os = "macos")]
        inspector_argument: None,
        watchdog_shutdown: Mutex::new(None),
        watchdog_task: Mutex::new(None),
        exit_watchdog_shutdown: Mutex::new(None),
        exit_watchdog_task: Mutex::new(None),
        crashpad_guard_enabled: Arc::new(AtomicBool::new(false)),
        crashpad_guard_shutdown: Mutex::new(None),
        crashpad_guard_task: Mutex::new(None),
        local_router: Some(router),
    };
    let original = runtime.subagent_reconcile_config(&config).unwrap();
    let model = &original.subagent_roles["codey_worker"].model;
    let target = || {
        snapshot
            .read()
            .unwrap()
            .target_for_request(model, Some("route-b"), None)
            .unwrap()
            .provider_id
    };
    assert_eq!(model, "gpt-6-astra");
    assert_eq!(target(), "route-a");

    // Role edits and reordered model lists still work with the original map.
    let mut roles = config.clone();
    roles.profiles.reverse();
    roles
        .selected_models_by_provider
        .get_mut("route-a")
        .unwrap()
        .reverse();
    roles.subagent_roles.get_mut("codey_worker").unwrap().model = "route-a/vendor/model".into();
    roles
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .reasoning_effort = "low".into();
    runtime.sync_local_router_routes(&roles).unwrap();
    let reloaded = runtime.subagent_reconcile_config(&roles).unwrap();
    assert_eq!(
        reloaded.subagent_roles["codey_worker"].model,
        "vendor/model"
    );
    assert_eq!(
        reloaded.subagent_roles["codey_worker"].reasoning_effort,
        "low"
    );

    let mut changed = config.clone();
    changed
        .selected_models_by_provider
        .get_mut("route-b")
        .unwrap()
        .push(model.clone());
    assert!(
        runtime
            .sync_local_router_routes(&changed)
            .unwrap_err()
            .to_string()
            .contains("需重启")
    );
    assert!(runtime.subagent_reconcile_config(&changed).is_err());
    assert_eq!(target(), "route-a");

    // Removing A, or disabling optimization in saved settings, cannot release
    // the routing protection for children already running with raw model IDs.
    changed.profiles[0].enabled = false;
    changed.subagent_optimization = false;
    assert!(runtime.sync_local_router_routes(&changed).is_err());
    assert!(runtime.subagent_reconcile_config(&changed).is_err());
    assert_eq!(target(), "route-a");

    // Route-qualified role IDs remain safe when a custom catalog was installed.
    runtime.subagent_route_catalog_installed = true;
    runtime.sync_local_router_routes(&changed).unwrap();
    assert_eq!(target(), "route-b");
    runtime.subagent_route_catalog_installed = false;
    runtime.applied_config.subagent_optimization = false;
    runtime.sync_local_router_routes(&config).unwrap();
    assert_eq!(target(), "route-a");
    runtime.local_router.as_ref().unwrap().stop().await.unwrap();
}
