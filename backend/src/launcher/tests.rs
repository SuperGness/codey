use super::*;
use std::collections::HashMap;
use std::path::Path;

#[test]
fn packaged_activation_detects_a_reused_process_id() {
    let existing_process_ids = HashSet::from([41_u32, 42_u32]);

    assert!(activation_reused_existing_process(
        &existing_process_ids,
        42
    ));
    assert!(!activation_reused_existing_process(
        &existing_process_ids,
        43
    ));
}

#[test]
fn process_creation_identity_rejects_pid_reuse_when_timestamps_are_available() {
    assert!(process_creation_identity_matches(Some(100), Some(100)));
    assert!(!process_creation_identity_matches(Some(100), Some(101)));
    assert!(process_creation_identity_matches(Some(100), None));
    assert!(process_creation_identity_matches(None, Some(101)));
}

#[test]
fn windows_stop_survivors_match_targets_by_creation_identity() {
    let mut expected = HashMap::new();
    expected.insert(10, Some(100));
    expected.insert(11, Some(101));
    expected.insert(12, None);
    let targets = HashSet::from([10, 11, 12, 13]);
    let current = vec![
        (10, "ChatGPT.exe".to_string(), Some(100)),
        // A recycled pid no longer matches the process we tried to stop.
        (11, "ChatGPT.exe".to_string(), Some(999)),
        // The snapshot had no identity. Even if a timestamp is now available,
        // keep waiting without passing that new identity to retry termination.
        (12, "codex.exe".to_string(), Some(777)),
        // Processes outside the pre-termination snapshot are never ours.
        (13, "unrelated.exe".to_string(), None),
    ];

    let survivors = windows_stop_survivors(&expected, &current, &targets);
    assert_eq!(
        survivors,
        vec![
            (10, "ChatGPT.exe".to_string(), Some(100)),
            (12, "codex.exe".to_string(), None),
        ]
    );
}

#[test]
fn windows_owned_process_ids_include_descendants_from_snapshot() {
    let app_dir = Path::new(r"C:\Users\kim\AppData\Local\OpenAI Codex");
    let owned_executable = Path::new(r"C:\Users\kim\AppData\Local\OpenAI Codex\ChatGPT.exe");
    let unrelated_executable = Path::new(r"C:\Other\ChatGPT.exe");
    let processes = [
        (10, 0, Some(owned_executable)),
        (11, 10, None),
        (12, 11, None),
        (13, 0, Some(unrelated_executable)),
        (14, 13, None),
    ];

    let process_ids = windows_owned_process_ids_from_snapshot(app_dir, None, processes);

    assert!(process_ids.contains(&10));
    assert!(process_ids.contains(&11));
    assert!(process_ids.contains(&12));
    assert!(!process_ids.contains(&13));
    assert!(!process_ids.contains(&14));
}

#[test]
fn windows_stop_failure_summary_lists_surviving_executables() {
    let remaining = vec![
        (10, "ChatGPT.exe".to_string(), Some(100)),
        (11, "codex.exe".to_string(), Some(101)),
    ];
    assert_eq!(
        windows_stop_failure_summary(&remaining),
        "2 个进程仍在运行：ChatGPT.exe(10)、codex.exe(11)",
    );

    let many = (0..7)
        .map(|index| (100 + index, "helper.exe".to_string(), None))
        .collect::<Vec<_>>();
    let summary = windows_stop_failure_summary(&many);
    assert!(summary.starts_with("7 个进程仍在运行："));
    assert!(summary.ends_with("等共 7 个"));
}

#[test]
fn windows_stop_tracking_absorbs_late_descendants() {
    let mut process_ids = HashSet::from([10]);

    windows_extend_tracked_descendants_from_snapshot(
        &mut process_ids,
        vec![(10, 0, None), (11, 10, None), (12, 11, None), (20, 0, None)],
    );

    assert_eq!(process_ids, HashSet::from([10, 11, 12]));
}

#[test]
fn official_provider_inherits_the_codex_builtin_model_catalog() {
    assert!(!should_install_codey_model_catalog(true, true));
    assert!(!should_install_codey_model_catalog(true, false));
}

#[test]
fn third_party_provider_installs_the_codey_model_catalog_when_available() {
    assert!(should_install_codey_model_catalog(false, true));
    assert!(!should_install_codey_model_catalog(false, false));
}

#[tokio::test]
async fn startup_fallback_removes_search_from_a_stale_chat_route_catalog() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(model_catalog::relative_path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "models": [{
                "slug": "route-chat/gpt-5.6-sol",
                "display_name": "Chat / GPT-5.6-Sol",
                "description": "Third-party API model",
                "base_instructions": "test instructions",
                "codey_source": "third_party",
                "supports_search_tool": true,
                "web_search_tool_type": "text_and_image"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut route = ProviderProfile::new("Chat route");
    route.id = "route-chat".into();
    route.base_url = "https://chat.example/v1".into();
    route.api_key = "secret".into();
    route.api_key_configured = true;
    route.upstream_protocol = crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    route.normalize();
    let mut config = CodeyConfig {
        local_router_enabled: true,
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        ..CodeyConfig::default()
    };
    config
        .upstream_models_by_provider
        .insert("route-chat".into(), vec!["gpt-5.6-sol".into()]);
    config
        .selected_models_by_provider
        .insert("route-chat".into(), vec!["gpt-5.6-sol".into()]);
    config = config.normalize();

    let startup = prepare_startup_model_catalog(&config, &config.profiles[0], home.path())
        .await
        .unwrap();

    assert!(startup.use_official_catalog);
    let catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(catalog["models"][0].get("supports_search_tool").is_none());
    assert!(catalog["models"][0].get("web_search_tool_type").is_none());
}

#[test]
fn generated_catalog_uses_the_route_aware_default_selector() {
    let config = CodeyConfig {
        default_model: "route-a/shared-model".into(),
        ..CodeyConfig::default()
    };
    let state = model_catalog::ModelSelectionState {
        default_model: "shared-model".into(),
        ..model_catalog::ModelSelectionState::default()
    };

    assert_eq!(
        runtime_default_model(&config, true, &state).as_deref(),
        Some("route-a/shared-model")
    );
    assert_eq!(
        runtime_default_model(&config, false, &state).as_deref(),
        Some("shared-model")
    );

    let mut official = ProviderProfile::new("Official");
    official.source_provider_id = Some("openai".into());
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    let mut official_config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official],
        default_model: "openai/gpt-5.6-sol".into(),
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    };
    official_config
        .selected_models_by_provider
        .insert("openai".into(), vec!["gpt-5.6-sol".into()]);
    assert_eq!(
        runtime_default_model(&official_config, true, &state).as_deref(),
        Some("gpt-5.6-sol")
    );
}

#[test]
fn subagent_runtime_models_use_route_aware_aliases() {
    let target = |provider: &str, model: &str, official: bool| RuntimeModelTarget {
        route_id: provider.into(),
        provider_id: provider.into(),
        alias: local_router::model_alias(provider, model),
        request_provider_id: ROUTER_PROVIDER_ID.into(),
        request_model: model.into(),
        upstream_model: model.into(),
        official,
    };
    let targets = vec![
        target("route-b", "shared-model", false),
        target("route-b", "vendor/model", false),
        target("openai", "gpt-5.6-sol", true),
    ];
    for (requested, expected) in [
        ("shared-model", "route-b/shared-model"),
        (" SHARED-MODEL ", "route-b/shared-model"),
        ("route-b/shared-model", "route-b/shared-model"),
        ("vendor/model", "route-b/vendor/model"),
        ("gpt-5.6-sol", "gpt-5.6-sol"),
        ("unknown-model", "route-a/unknown-model"),
    ] {
        assert_eq!(
            route_subagent_model("route-a", requested, &targets, false),
            expected,
            "requested: {requested}"
        );
    }

    let mut ambiguous = targets.clone();
    ambiguous.push(target("route-a", "shared-model", false));
    assert_eq!(
        route_subagent_model("route-a", "shared-model", &ambiguous, false),
        "route-a/shared-model"
    );
    assert_eq!(
        route_subagent_model("route-a", "route-b/shared-model", &ambiguous, false),
        "route-b/shared-model"
    );

    let official_targets = vec![target("openai", "gpt-5.6-sol", true)];
    for requested in ["gpt-5.6-sol", "openai/gpt-5.6-sol"] {
        assert_eq!(
            route_subagent_model("openai", requested, &official_targets, true),
            "gpt-5.6-sol"
        );
    }
    assert_eq!(
        route_subagent_model("openai", "unknown-model", &official_targets, true),
        "unknown-model"
    );
}

#[test]
fn native_subagent_runtime_models_use_upstream_model_ids() {
    let mut route = ProviderProfile::new("Relay");
    route.id = "route-a".to_string();
    route.short_name = "R".to_string();
    route.base_url = "https://relay.example/v1".to_string();
    route.api_key = "secret".to_string();
    route.normalize();
    let mut config = CodeyConfig {
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        subagent_model: local_router::model_alias("route-a", "shared-model"),
        ..CodeyConfig::default()
    };
    config.selected_models_by_provider.insert(
        "route-a".to_string(),
        vec!["shared-model".to_string(), "worker-model".to_string()],
    );
    config.subagent_roles.get_mut("codey_worker").unwrap().model =
        local_router::model_alias("route-a", "worker-model");
    config
        .subagent_roles
        .get_mut("codey_quick_scan")
        .unwrap()
        .model = local_router::model_alias("route-a", "gpt-5.6-terra");
    config
        .subagent_roles
        .get_mut("codey_visual_worker")
        .unwrap()
        .model = "custom/gpt-5.6-luna".to_string();
    let native = native_subagent_runtime_config(&config);

    assert_eq!(native.subagent_model, "shared-model");
    assert_eq!(native.subagent_roles["codey_worker"].model, "worker-model");
    assert_eq!(
        native.subagent_roles["codey_quick_scan"].model,
        "gpt-5.6-terra"
    );
    assert_eq!(
        native.subagent_roles["codey_visual_worker"].model,
        "custom/gpt-5.6-luna"
    );
}

#[test]
fn native_subagent_runtime_strips_official_provider_prefix_without_route_targets() {
    let mut route = ProviderProfile::new("Custom");
    route.id = "custom".to_string();
    route.short_name = "C".to_string();
    route.base_url = "https://relay.example/v1".to_string();
    route.api_key = "secret".to_string();
    route.normalize();
    let mut config = CodeyConfig {
        active_profile_id: route.id.clone(),
        local_router_enabled: false,
        profiles: vec![route],
        subagent_model: "custom/gpt-5.6-sol".into(),
        subagent_roles: crate::config::uniform_subagent_roles("custom/gpt-5.6-sol", "high"),
        ..CodeyConfig::default()
    };
    config
        .subagent_roles
        .get_mut("codey_quick_scan")
        .unwrap()
        .model = "custom/gpt-5.6-terra".to_string();
    config
        .subagent_roles
        .get_mut("codey_visual_worker")
        .unwrap()
        .model = "custom/gpt-5.6-luna".to_string();
    config
        .subagent_roles
        .get_mut("codey_deep_research")
        .unwrap()
        .model = "custom/vendor/model".to_string();
    let native = native_subagent_runtime_config(&config);

    assert_eq!(native.subagent_model, "gpt-5.6-sol");
    assert_eq!(
        native.subagent_roles["codey_quick_scan"].model,
        "gpt-5.6-terra"
    );
    assert_eq!(
        native.subagent_roles["codey_visual_worker"].model,
        "gpt-5.6-luna"
    );
    assert_eq!(
        native.subagent_roles["codey_deep_research"].model,
        "custom/vendor/model"
    );
}

#[test]
fn native_subagent_runtime_uses_synced_upstream_model_without_route_targets() {
    let mut route = ProviderProfile::new("Custom");
    route.id = "custom".to_string();
    route.base_url = "https://relay.example/v1".to_string();
    route.api_key = "secret".to_string();
    route.normalize();
    let mut config = CodeyConfig {
        active_profile_id: route.id.clone(),
        local_router_enabled: false,
        profiles: vec![route],
        subagent_model: "custom/vendor/model".into(),
        ..CodeyConfig::default()
    };
    config
        .upstream_models_by_provider
        .insert("custom".into(), vec!["vendor/model".into()]);

    let native = native_subagent_runtime_config(&config);

    assert_eq!(native.subagent_model, "vendor/model");
}

#[test]
fn native_subagent_runtime_follows_the_current_provider_models_and_efforts() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"model_provider = "route-a"

[model_providers.route-a]
name = "Route A"
base_url = "https://route-a.example/v1"
wire_api = "responses"
experimental_bearer_token = "secret"
"#,
    )
    .unwrap();
    let mut route_a = ProviderProfile::new("Route A");
    route_a.id = "route-a".into();
    route_a.base_url = "https://route-a.example/v1".into();
    route_a.api_key = "secret".into();
    route_a.normalize();
    let mut route_b = ProviderProfile::new("Route B");
    route_b.id = "route-b".into();
    route_b.base_url = "https://route-b.example/v1".into();
    route_b.api_key = "secret".into();
    route_b.normalize();
    let config = CodeyConfig {
        local_router_enabled: false,
        active_profile_id: route_b.id.clone(),
        profiles: vec![route_a, route_b],
        selected_models_by_provider: std::collections::BTreeMap::from([
            ("route-a".into(), vec!["model-a".into()]),
            ("route-b".into(), vec!["model-b".into()]),
        ]),
        upstream_models_by_provider: std::collections::BTreeMap::from([
            ("route-a".into(), vec!["model-a".into()]),
            ("route-b".into(), vec!["model-b".into()]),
        ]),
        subagent_model: "route-b/model-b".into(),
        subagent_reasoning_effort: "max".into(),
        subagent_roles: crate::config::uniform_subagent_roles("route-b/model-b", "max"),
        ..CodeyConfig::default()
    };

    let native = reconciled_native_subagent_runtime_config(&config, home.path());

    assert_eq!(native.subagent_model, "model-a");
    assert_eq!(native.subagent_reasoning_effort, "low");
    assert!(
        native.subagent_roles.values().all(|selection| {
            selection.model == "model-a" && selection.reasoning_effort == "low"
        })
    );
}

#[test]
fn persistent_session_provider_never_targets_the_launch_only_router_id() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        format!("model_provider = \"{ROUTER_PROVIDER_ID}\"\n"),
    )
    .unwrap();

    assert_eq!(persistent_session_provider(temp.path()).unwrap(), "openai");
}

#[test]
fn persistent_session_provider_rejects_a_user_owned_router_before_sync() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        format!(
            "[model_providers.{ROUTER_PROVIDER_ID}]\n\
             name = \"User Router\"\n\
             base_url = \"https://example.com/v1\"\n"
        ),
    )
    .unwrap();

    let error = persistent_session_provider(temp.path()).unwrap_err();
    assert!(error.to_string().contains("已占用 Codey 内部 Provider ID"));
}

#[test]
fn persistent_session_provider_allows_a_codey_owned_resume_shim() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        format!(
            "model_provider = \"codey_global\"\n\
             \n\
             [model_providers.codey_global]\n\
             name = \"OpenAI\"\n\
             base_url = \"https://chatgpt.com/backend-api/codex\"\n\
             wire_api = \"responses\"\n\
             requires_openai_auth = true\n\
             \n\
             [model_providers.{ROUTER_PROVIDER_ID}]\n\
             name = \"Codey Local Router\"\n\
             base_url = \"https://chatgpt.com/backend-api/codex\"\n\
             wire_api = \"responses\"\n\
             requires_openai_auth = true\n\
             supports_websockets = false\n"
        ),
    )
    .unwrap();

    assert_eq!(
        persistent_session_provider(temp.path()).unwrap(),
        "codey_global"
    );
}

#[test]
fn persistent_session_provider_reads_legacy_codey_global_after_resume_shim() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        "model_provider = \"codey_global\"\n\
         \n\
         [model_providers]\n\
         \n\
         [model_providers.codey_global]\n\
         name = \"OpenAI\"\n\
         base_url = \"https://chatgpt.com/backend-api/codex\"\n\
         wire_api = \"responses\"\n\
         requires_openai_auth = true\n",
    )
    .unwrap();

    assert!(crate::codex_config::prepare_persistent_router_resume_shim_at(temp.path()).unwrap());
    assert_eq!(
        persistent_session_provider(temp.path()).unwrap(),
        "codey_global"
    );
}

#[test]
fn session_maintenance_repairs_router_stamped_threads_for_third_party_launches() {
    // Third-party users may never set `model_provider` in config.toml, and a
    // Codex started outside Codey cannot resolve the launch-only router id
    // that Codey stamped into their thread records.
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        "model = \"gpt-5.6-terra\"\n",
    )
    .unwrap();
    let sessions = temp.path().join("sessions/2026/08/27");
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join("rollout-thread-1.jsonl");
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "thread-1",
                    "model_provider": ROUTER_PROVIDER_ID
                }
            })
        ),
    )
    .unwrap();

    let provider = persistent_session_provider(temp.path()).unwrap();
    assert_eq!(provider, "openai");
    let result =
        codey_runtime_data::run_provider_sync_with_target(Some(temp.path()), Some(&provider));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.target_provider, "openai");
    let repaired = std::fs::read_to_string(&rollout).unwrap();
    assert!(repaired.contains("\"model_provider\":\"openai\""));
    assert!(!repaired.contains(ROUTER_PROVIDER_ID));
}

#[test]
fn session_maintenance_repairs_a_runtime_only_provider_to_the_persistent_provider() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("config.toml"),
        "model_provider = \"openai\"\n",
    )
    .unwrap();
    let sessions = temp.path().join("sessions/2026/08/24");
    std::fs::create_dir_all(&sessions).unwrap();
    let rollout = sessions.join("rollout-thread-1.jsonl");
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "thread-1",
                    "model_provider": "route-runtime-only"
                }
            })
        ),
    )
    .unwrap();

    let provider = persistent_session_provider(temp.path()).unwrap();
    let result =
        codey_runtime_data::run_provider_sync_with_target(Some(temp.path()), Some(&provider));

    assert_eq!(result.status, ProviderSyncStatus::Synced);
    assert_eq!(result.target_provider, "openai");
    let repaired = std::fs::read_to_string(rollout).unwrap();
    assert!(repaired.contains("\"model_provider\":\"openai\""));
    assert!(!repaired.contains("route-runtime-only"));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_launch_forces_a_new_app_instance() {
    let command = build_fresh_macos_open_command(
        std::path::Path::new("/Applications/ChatGPT.app"),
        9229,
        &["--inspect-brk=127.0.0.1:19321".to_string()],
    );
    assert_eq!(command.first().map(String::as_str), Some("open"));
    assert!(command.iter().any(|part| part == "-n"));
    assert!(command.iter().any(|part| part == "-W"));
    assert!(
        command
            .iter()
            .any(|part| part == "--remote-debugging-port=9229")
    );
    assert!(
        command
            .iter()
            .any(|part| part == "--inspect-brk=127.0.0.1:19321")
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_running_check_does_not_match_an_unrelated_app_path() {
    let running = macos_codex_is_running(std::path::Path::new(
        "/Applications/Definitely Not Codex.app",
    ))
    .await
    .unwrap();
    assert!(!running);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_running_check_matches_only_the_app_main_executable() {
    let processes = crate::process_tree::parse_unix_process_snapshot(
        b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT --remote-debugging-port=9229\n\
          101 100 100 Thu Jul 23 19:23:13 2026 /Applications/ChatGPT.app/Contents/Resources/codex app-server\n\
          102 101 102 Thu Jul 23 19:23:14 2026 /Applications/ChatGPT.app/Contents/Frameworks/Chromium Helper\n",
    );
    assert!(macos_main_executable_is_running(
        &processes,
        Path::new("/Applications/ChatGPT.app/Contents/MacOS/ChatGPT"),
    ));
    assert!(!macos_main_executable_is_running(
        &processes[1..],
        Path::new("/Applications/ChatGPT.app/Contents/MacOS/ChatGPT"),
    ));
}

#[test]
fn owned_codex_tree_includes_bundle_helpers_and_external_descendants() {
    let processes = crate::process_tree::parse_unix_process_snapshot(
        b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT --inspect\n\
          101 100 100 Thu Jul 23 19:23:13 2026 /Applications/ChatGPT.app/Contents/Resources/codex app-server\n\
          102 101 102 Thu Jul 23 19:23:14 2026 node ./mcp/server.mjs\n\
          103 1 103 Thu Jul 23 19:23:15 2026 /Applications/ChatGPT.app/Contents/Frameworks/browser_crashpad_handler\n\
          200 1 200 Thu Jul 23 19:23:16 2026 unrelated\n",
    );
    assert_eq!(
        owned_unix_codex_process_ids(
            &processes,
            Path::new("/Applications/ChatGPT.app"),
            None,
            None,
            Some("--inspect"),
        ),
        HashSet::from([100, 101, 102, 103])
    );
}

#[tokio::test]
async fn unix_shutdown_terminates_the_spawned_process_group() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 & wait"]);
    command.process_group(0);
    let mut child = command.spawn().expect("spawn process tree");
    let process_id = child.id().expect("child process id");

    terminate_unix_codex_processes(
        Path::new("/definitely-not-a-real-codex-app"),
        Some(process_id),
        Some(process_id),
        None,
    )
    .await
    .expect("terminate process tree");

    tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .expect("root process was left running")
        .expect("wait for root process");
}

#[tokio::test]
async fn exit_watcher_reports_a_naturally_exited_child() {
    let child = Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn short-lived child");
    let child = Arc::new(Mutex::new(Some(child)));
    let exited = Arc::new(AtomicBool::new(false));
    let (_shutdown, exit_rx, task) = spawn_codex_exit_watcher(child, exited.clone());

    tokio::time::timeout(Duration::from_secs(2), exit_rx)
        .await
        .expect("watcher timed out")
        .expect("watcher was cancelled");
    task.await.expect("watcher task failed");
    assert!(exited.load(Ordering::Acquire));
}

#[tokio::test]
async fn exit_watcher_returns_the_child_to_stop_on_shutdown() {
    let child = Command::new("sh")
        .args(["-c", "sleep 30"])
        .spawn()
        .expect("spawn long-lived child");
    let child = Arc::new(Mutex::new(Some(child)));
    let exited = Arc::new(AtomicBool::new(false));
    let (shutdown, _exit_rx, task) = spawn_codex_exit_watcher(child.clone(), exited.clone());

    shutdown.send(()).expect("send watcher shutdown");
    task.await.expect("watcher task failed");

    assert!(!exited.load(Ordering::Acquire));
    let mut process = child
        .lock()
        .await
        .take()
        .expect("watcher should return the child");
    process.kill().await.expect("kill child");
    process.wait().await.expect("reap child");
}

#[test]
fn cdp_watchdog_requires_consecutive_failures_before_reinjecting() {
    let mut failures = 0;

    assert!(!watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Unhealthy
    ));
    assert_eq!(failures, 1);
    assert!(!watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Healthy
    ));
    assert_eq!(failures, 0);
    assert!(!watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Unhealthy
    ));
    assert!(watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Unhealthy
    ));
}

#[test]
fn cdp_watchdog_does_not_reinject_after_renderer_timeouts() {
    let mut failures = 0;

    assert!(!watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Inconclusive
    ));
    assert!(!watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Inconclusive
    ));
    assert_eq!(failures, 0);

    assert!(!watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Unhealthy
    ));
    assert!(!watchdog_should_reinject(
        &mut failures,
        InjectionHealth::Inconclusive
    ));
    assert_eq!(failures, 0);
}

#[test]
fn cdp_watchdog_immediately_rediscovers_an_unavailable_target() {
    let mut failures = 1;

    assert!(watchdog_should_reinject(
        &mut failures,
        InjectionHealth::TargetUnavailable
    ));
    assert_eq!(failures, 0);
}
