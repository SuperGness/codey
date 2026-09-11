use super::*;

#[tokio::test]
async fn pet_state_failure_does_not_abort_startup_or_replace_state() {
    for slim_codex_pet in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join(".codex-global-state.json");
        let backup = temp.path().join(".codex-global-state.json.bak");
        std::fs::write(&primary, b"{broken").unwrap();
        std::fs::write(&backup, b"{also broken").unwrap();
        let config = CodeyConfig {
            slim_codex_pet,
            ..CodeyConfig::default()
        };

        let patch = prepare_startup_patches(temp.path(), &config).await;

        assert_ne!(patch.debug_port, 0);
        assert_eq!(std::fs::read(&primary).unwrap(), b"{broken");
        assert_eq!(std::fs::read(&backup).unwrap(), b"{also broken");
    }
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
        startup_injection_mode: "node_options".to_string(),
    };
    let value = serde_json::to_value(status).unwrap();

    assert_eq!(value["sessionFilesFixed"], 0);
    assert_eq!(value["sqliteRowsUpdated"], 0);
    assert_eq!(value["ghostTasksPruned"], 2);
    assert_eq!(value["startupInjectionMode"], "node_options");
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
