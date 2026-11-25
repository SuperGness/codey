use super::*;

#[tokio::test]
async fn pet_state_failure_does_not_abort_startup_or_replace_state() {
    for slim_codex_pet in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join(".codex-global-state.json");
        let backup = temp.path().join(".codex-global-state.json.bak");
        std::fs::write(&primary, b"{broken").unwrap();
        std::fs::write(&backup, b"{also broken").unwrap();

        apply_startup_pet_slim(temp.path(), slim_codex_pet).await;
        let patch = select_startup_debug_port().unwrap();

        assert_ne!(patch.debug_port, 0);
        assert_eq!(std::fs::read(&primary).unwrap(), b"{broken");
        assert_eq!(std::fs::read(&backup).unwrap(), b"{also broken");
    }
}

#[test]
fn debug_port_failure_aborts_before_startup_patches() {
    let temp = tempfile::tempdir().unwrap();
    let result = select_startup_debug_port_with(|| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "loopback binding denied",
        ))
    });
    let error = match result {
        Ok(_) => panic!("startup should fail without a debug port"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("loopback binding denied"));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn debug_port_zero_never_reaches_startup_command() {
    let temp = tempfile::tempdir().unwrap();
    let result = select_startup_debug_port_with(|| Ok(0));
    assert!(result.is_err());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn maintenance_status_exposes_structured_session_metrics() {
    let cleanup = Ok(SessionIndexCleanupReport {
        scanned_entries: 5,
        live_threads: 3,
        pruned_entries: 2,
        backup_dir: None,
    });

    let summary = session_maintenance_summary(&cleanup);
    let status = MaintenanceStatus {
        session_status: summary.status,
        session_files_fixed: summary.files_fixed,
        sqlite_rows_updated: summary.sqlite_rows_updated,
        ghost_tasks_pruned: summary.ghost_tasks_pruned,
        performance_status: "ready".to_string(),
        performance_detail: String::new(),
        startup_injection_mode: "cli".to_string(),
    };
    let value = serde_json::to_value(status).unwrap();

    assert_eq!(value["sessionFilesFixed"], 0);
    assert_eq!(value["sqliteRowsUpdated"], 0);
    assert_eq!(value["ghostTasksPruned"], 2);
    assert_eq!(value["startupInjectionMode"], "cli");
}

#[test]
fn startup_without_provider_rewrites_is_still_ready() {
    let cleanup = Ok(SessionIndexCleanupReport {
        scanned_entries: 0,
        live_threads: 0,
        pruned_entries: 0,
        backup_dir: None,
    });

    let summary = session_maintenance_summary(&cleanup);

    assert_eq!(summary.status, "ready");
    assert_eq!(summary.files_fixed, 0);
    assert_eq!(summary.sqlite_rows_updated, 0);
}

#[test]
fn runtime_config_matches_agrees_with_from_config_equality() {
    let mut applied = CodeyConfig::default();
    applied.profiles[0].name = "主线路".to_string();
    applied
        .selected_models_by_provider
        .insert("openai".to_string(), vec!["gpt-6-astra".to_string()]);
    applied.subagent_model = "gpt-6-astra".to_string();
    let models = RuntimeModelConfig::from_config(&applied);
    let subagent = RuntimeSubagentConfig::from_config(&applied);
    assert!(models.matches(&applied));
    assert!(subagent.matches(&applied));

    let mut changed = applied.clone();
    changed.profiles[0].enabled = !changed.profiles[0].enabled;
    assert_eq!(
        models.matches(&changed),
        models == RuntimeModelConfig::from_config(&changed)
    );
    assert!(!models.matches(&changed));

    let mut changed = applied.clone();
    changed.default_model = "gpt-5.6-luna".to_string();
    assert!(!models.matches(&changed));

    let mut changed = applied.clone();
    changed.model_ultrafast_by_provider.insert(
        "relay".into(),
        std::collections::BTreeMap::from([("gpt-6-astra".into(), true)]),
    );
    assert!(!models.matches(&changed));
    let mut changed = applied.clone();
    changed.upstream_speed_tiers_by_provider.insert(
        "openai".into(),
        std::collections::BTreeMap::from([(
            "gpt-6-astra".into(),
            model_catalog::ModelSpeedTiers {
                additional_speed_tiers: vec!["ultrafast".into()],
                ..Default::default()
            },
        )]),
    );
    assert!(!models.matches(&changed));

    let mut changed = applied.clone();
    changed.profiles.push(changed.profiles[0].clone());
    assert!(!models.matches(&changed));

    let mut changed = applied.clone();
    changed.subagent_reasoning_effort = format!("{}-changed", applied.subagent_reasoning_effort);
    assert_eq!(
        subagent.matches(&changed),
        subagent == RuntimeSubagentConfig::from_config(&changed)
    );
    assert!(!subagent.matches(&changed));
}
