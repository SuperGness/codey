use super::*;
use crate::config::ProviderProfile;

#[tokio::test]
async fn model_save_routes_accept_missing_or_null_ids_without_weakening_required_routes() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(CodeyConfig {
            local_router_enabled: true,
            profiles: Vec::new(),
            active_profile_id: "missing-route".into(),
            ..CodeyConfig::default()
        }),
        ..AppState::default()
    });
    for (command, payload, expected_message) in [
        (
            "save_selected_models",
            json!({ "officialModels": [], "thirdPartyModels": ["model-a"] }),
            "找不到要配置模型的线路",
        ),
        (
            "save_default_model",
            json!({ "model": "model-a" }),
            "找不到要设置默认模型的线路",
        ),
    ] {
        for route_id in [None, Some(Value::Null), Some(json!("missing-route"))] {
            let mut args = payload.clone();
            if let Some(route_id) = route_id {
                args["routeId"] = route_id;
            }
            let result = invoke_api(&state, command, args).await;
            assert_eq!(result["status"], "failed");
            // Reaching route lookup proves the request passed argument parsing;
            // a missing fixture route prevents any configuration writes.
            assert_eq!(result["message"], expected_message, "{command}");
        }
        for route_id in [json!(7), json!(false), json!([]), json!({})] {
            let mut args = payload.clone();
            args["routeId"] = route_id;
            let result = invoke_api(&state, command, args).await;
            assert_eq!(result["status"], "failed");
            assert!(
                result["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("参数 routeId 无效")
            );
        }
    }
    let result = invoke_api(
        &state,
        "delete_route",
        json!({ "routeId": null, "expectedRevision": 0 }),
    )
    .await;
    assert_eq!(result["message"], "缺少参数：routeId");
}

#[test]
fn bridge_field_helpers_preserve_existing_payload_semantics() {
    let payload = json!({
        "text": "  value  ",
        "offset": 42,
        "wrongText": 7,
        "wrongOffset": "42",
        "items": [" first ", 7, "", "second", "third"],
    });

    assert_eq!(bridge_string(&payload, "text"), "  value  ");
    assert_eq!(bridge_string(&payload, "missing"), "");
    assert_eq!(bridge_string(&payload, "wrongText"), "");
    assert_eq!(bridge_u64(&payload, "offset"), Some(42));
    assert_eq!(bridge_u64(&payload, "missing"), None);
    assert_eq!(bridge_u64(&payload, "wrongOffset"), None);
    assert_eq!(
        bridge_string_array(&payload, "items", 2),
        vec!["first".to_string(), "second".to_string()]
    );
    assert!(bridge_string_array(&payload, "missing", 2).is_empty());
}

#[test]
fn user_fastctx_prevents_enabling_the_embedded_tools() {
    let available = FastContextToolsStatus::default();
    assert!(embedded_fast_context_tools_enabled(true, &available));
    assert!(!embedded_fast_context_tools_enabled(false, &available));

    let user_configured = FastContextToolsStatus {
        user_configured: true,
        detection_failed: false,
        server_id: Some("fastctx".to_string()),
    };
    assert!(!embedded_fast_context_tools_enabled(true, &user_configured));

    let detection_failed = FastContextToolsStatus {
        user_configured: false,
        detection_failed: true,
        server_id: None,
    };
    assert!(!embedded_fast_context_tools_enabled(
        true,
        &detection_failed
    ));
    assert_eq!(
        fast_context_tools_status_or_blocked::<&str>(Err("invalid config")),
        detection_failed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_metadata_cache_operations_are_serialized_in_blocking_workers() {
    let state = Arc::new(AppState::default());
    let first_started = Arc::new(AtomicBool::new(false));
    let second_started = Arc::new(AtomicBool::new(false));
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));

    let first = tokio::spawn({
        let state = Arc::clone(&state);
        let first_started = Arc::clone(&first_started);
        let release = Arc::clone(&release);
        async move {
            with_session_metadata_cache(&state, "first cache operation", move |_| {
                first_started.store(true, Ordering::Release);
                let (released, signal) = &*release;
                let guard = released
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let guard = signal
                    .wait_while(guard, |released| !*released)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                drop(guard);
                1
            })
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !first_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first cache operation should start");

    let second_contended = state.session_metadata_cache_contended.notified();
    let second = tokio::spawn({
        let state = Arc::clone(&state);
        let second_started = Arc::clone(&second_started);
        async move {
            with_session_metadata_cache(&state, "second cache operation", move |_| {
                second_started.store(true, Ordering::Release);
                2
            })
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), second_contended)
        .await
        .expect("the second cache operation did not contend for exclusive ownership");
    assert!(
        !second_started.load(Ordering::Acquire),
        "the second operation must wait for exclusive cache ownership"
    );

    let (released, signal) = &*release;
    *released
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    signal.notify_all();

    assert_eq!(first.await.unwrap().unwrap(), 1);
    assert_eq!(second.await.unwrap().unwrap(), 2);
    assert!(second_started.load(Ordering::Acquire));
}

#[test]
fn renderer_settings_keep_api_keys_and_clear_notification_secrets() {
    let mut config = CodeyConfig::default();
    config.profiles[0].api_key = "renderer-secret".to_string();
    config.prompt_optimization.api_key = "optimizer-secret".to_string();
    config.hide_full_access_warning = true;
    config.webhook.url = "https://open.feishu.cn/legacy-secret".to_string();
    config.webhook.channels.push(NotificationChannelConfig {
        id: "feishu-1".to_string(),
        url: "https://open.feishu.cn/open-apis/bot/v2/hook/feishu-secret".to_string(),
        ..NotificationChannelConfig::default()
    });
    config.webhook.channels.push(NotificationChannelConfig {
        id: "telegram-1".to_string(),
        kind: crate::notifications::NotificationChannelKind::Telegram,
        bot_token: "telegram-secret".to_string(),
        chat_id: "-100123".to_string(),
        ..NotificationChannelConfig::default()
    });
    config.webhook.channels.push(NotificationChannelConfig {
        id: "wecom-1".to_string(),
        kind: crate::notifications::NotificationChannelKind::Wecom,
        url: "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=wecom-secret".to_string(),
        ..NotificationChannelConfig::default()
    });
    config.webhook.channels.push(NotificationChannelConfig {
        id: "wechat-claw-1".to_string(),
        kind: crate::notifications::NotificationChannelKind::WechatClaw,
        url: "https://ilinkai.weixin.qq.com".to_string(),
        bot_token: "wechat-claw-secret".to_string(),
        context_token: "wechat-context-secret".to_string(),
        chat_id: "user@im.wechat".to_string(),
        ..NotificationChannelConfig::default()
    });

    let public = serde_json::to_value(redacted_config(&config)).unwrap();

    assert_eq!(public["profiles"][0]["apiKey"], "renderer-secret");
    assert_eq!(public["profiles"][0]["apiKeyConfigured"], true);
    assert_eq!(public["promptOptimization"]["apiKey"], "optimizer-secret");
    assert_eq!(public["promptOptimization"]["apiKeyConfigured"], true);
    assert!(public["profiles"][0].get("clearApiKey").is_none());
    assert_eq!(public["hideFullAccessWarning"], true);
    assert!(public["webhook"].get("url").is_none());
    assert_eq!(public["webhook"]["channels"][0]["url"], "");
    assert_eq!(public["webhook"]["channels"][0]["urlConfigured"], true);
    assert_eq!(public["webhook"]["channels"][1]["botToken"], "");
    assert_eq!(public["webhook"]["channels"][1]["botTokenConfigured"], true);
    assert_eq!(public["webhook"]["channels"][2]["url"], "");
    assert_eq!(public["webhook"]["channels"][2]["urlConfigured"], true);
    assert_eq!(public["webhook"]["channels"][3]["botToken"], "");
    assert_eq!(public["webhook"]["channels"][3]["botTokenConfigured"], true);
    assert_eq!(public["webhook"]["channels"][3]["contextToken"], "");
    assert_eq!(
        public["webhook"]["channels"][3]["contextTokenConfigured"],
        true
    );
    assert!(public.to_string().contains("renderer-secret"));
    assert!(public.to_string().contains("optimizer-secret"));
    assert!(!public.to_string().contains("feishu-secret"));
    assert!(!public.to_string().contains("telegram-secret"));
    assert!(!public.to_string().contains("wecom-secret"));
    assert!(!public.to_string().contains("wechat-claw-secret"));
    assert!(!public.to_string().contains("wechat-context-secret"));
    assert!(!public.to_string().contains("legacy-secret"));
}

#[test]
fn provider_secret_merge_allows_changing_official_routes_to_api_key() {
    let mut official = crate::config::ProviderProfile::new("OpenAI 官方直登");
    official.id = "official-route".to_string();
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
    official.source_provider_id = Some("local-official".to_string());
    official.normalize();

    let previous = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official.clone()],
        ..CodeyConfig::default()
    };
    let mut input = official;
    input.auth_mode = crate::config::AUTH_MODE_API_KEY.to_string();
    input.upstream_protocol = crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string();
    input.base_url = "https://relay.example/v1".to_string();
    input.api_key = "sk-relay".to_string();
    input.api_key_configured = false;
    input.official_account = false;
    input.short_name = "中转".to_string();

    let merged = merge_profile_secrets(vec![input], &previous).unwrap();
    let route = &merged[0];

    assert_eq!(route.auth_mode, crate::config::AUTH_MODE_API_KEY);
    assert_eq!(
        route.upstream_protocol,
        crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    );
    assert_eq!(route.base_url, "https://relay.example/v1");
    assert_eq!(route.api_key, "sk-relay");
    assert_eq!(route.short_name, "中转");
    assert!(!route.official_account);
    assert!(route.source_provider_id.is_none());
    assert!(!route.supports_remote_compaction);
    assert!(!route.supports_websockets);
}

#[test]
fn account_usage_stays_enabled_when_an_official_route_exists_but_a_third_party_route_is_active() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = "official-route".to_string();
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
    official.normalize();

    let mut third_party = ProviderProfile::new("第三方线路");
    third_party.id = "third-party-route".to_string();
    third_party.base_url = "https://relay.example/v1".to_string();
    third_party.api_key = "sk-relay".to_string();
    third_party.normalize();

    let config = CodeyConfig {
        active_profile_id: third_party.id.clone(),
        profiles: vec![official, third_party],
        show_account_usage_in_header: true,
        ..CodeyConfig::default()
    };

    assert!(account_usage_enabled_for_config(&config));
}

#[test]
fn account_usage_requires_both_the_display_setting_and_an_official_route() {
    let mut third_party = ProviderProfile::new("第三方线路");
    third_party.id = "third-party-route".to_string();
    third_party.base_url = "https://relay.example/v1".to_string();
    third_party.api_key = "sk-relay".to_string();
    third_party.normalize();

    let without_official = CodeyConfig {
        active_profile_id: third_party.id.clone(),
        profiles: vec![third_party],
        show_account_usage_in_header: true,
        ..CodeyConfig::default()
    };
    assert!(!account_usage_enabled_for_config(&without_official));

    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
    official.normalize();
    let disabled = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official],
        show_account_usage_in_header: false,
        ..CodeyConfig::default()
    };
    assert!(!account_usage_enabled_for_config(&disabled));
}

#[test]
fn native_account_usage_follows_the_current_official_provider_without_a_saved_route() {
    let native_official = CodeyConfig {
        local_router_enabled: false,
        profiles: Vec::new(),
        show_account_usage_in_header: true,
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    };
    assert!(account_usage_enabled_for_config(&native_official));

    let native_third_party = CodeyConfig {
        official_account_available_this_launch: false,
        ..native_official.clone()
    };
    assert!(!account_usage_enabled_for_config(&native_third_party));

    let router_enabled_without_official_route = CodeyConfig {
        local_router_enabled: true,
        official_account_available_this_launch: true,
        ..native_official
    };
    assert!(!account_usage_enabled_for_config(
        &router_enabled_without_official_route
    ));
}

#[test]
fn inconclusive_official_auth_probe_keeps_active_third_party_route() {
    let mut third_party = ProviderProfile::new("第三方线路");
    third_party.id = "custom".to_string();
    third_party.base_url = "https://relay.example/v1".to_string();
    third_party.api_key = "sk-relay".to_string();
    third_party.normalize();
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
    official.source_provider_id = Some("openai".to_string());
    official.normalize();
    let previous = CodeyConfig {
        active_profile_id: third_party.id.clone(),
        profiles: vec![third_party.clone()],
        default_model: "custom/gpt-5.6-sol".to_string(),
        initial_route_import_completed: true,
        ..CodeyConfig::default()
    }
    .normalize();

    let next = route_config_for_official_probe(
        &previous,
        OfficialAccountProfileStatus::Unknown {
            profile: official,
            reason: "probe unavailable".to_string(),
        },
    )
    .unwrap();

    assert_eq!(next.active_profile_id, third_party.id);
    assert!(
        next.profiles
            .iter()
            .all(|profile| !profile.official_account)
    );
    assert!(!next.official_account_available_this_launch);
    assert_eq!(
        next.official_account_status_this_launch,
        LaunchOfficialAccountStatus::Unknown
    );
    assert_eq!(next.default_model, "custom/gpt-5.6-sol");
}

#[tokio::test]
async fn settings_bridge_matches_the_redacted_config_contract() {
    let state = Arc::new(AppState::default());
    let mut config = state.config.read().await.clone();
    config.profiles[0].api_key = "bridge-provider-secret".to_string();
    config.webhook.channels.push(NotificationChannelConfig {
        id: "bridge-feishu".to_string(),
        url: "https://open.feishu.cn/open-apis/bot/v2/hook/bridge-secret".to_string(),
        ..NotificationChannelConfig::default()
    });
    let expected = serde_json::to_value(redacted_config(&config)).unwrap();
    *state.config.write().await = config;

    let actual = state
        .bridge_request("/settings/get".to_string(), json!({}))
        .await;

    assert_eq!(actual, expected);
    assert!(actual.to_string().contains("bridge-provider-secret"));
    assert!(!actual.to_string().contains("bridge-secret"));
}

#[tokio::test]
async fn backend_health_bridge_avoids_runtime_status_collection() {
    let state = Arc::new(AppState::default());

    let actual = state
        .bridge_request("/backend/health".to_string(), json!({}))
        .await;

    assert_eq!(actual, json!({"status": "ok"}));
}

#[test]
fn completion_state_requires_the_exact_non_snapshot_terminal_turn() {
    let events = pending_approval::RecentSessionEvents {
        started_turns: Arc::new(vec![
            pending_approval::StartedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-completed".to_string(),
            },
            pending_approval::StartedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-running".to_string(),
            },
        ]),
        aborted_turns: Arc::new(vec![pending_approval::AbortedTurn {
            session_id: "session-1".to_string(),
            turn_id: "turn-aborted".to_string(),
            is_snapshot_replay: false,
        }]),
        completed_turns: Arc::new(vec![
            pending_approval::CompletedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-completed".to_string(),
                duration_ms: 10,
                completed_at: Some(42),
                error: None,
                is_snapshot_replay: false,
            },
            pending_approval::CompletedTurn {
                session_id: "session-1".to_string(),
                turn_id: "turn-imported".to_string(),
                duration_ms: 10,
                completed_at: Some(41),
                error: None,
                is_snapshot_replay: true,
            },
        ]),
        session_statuses: Arc::new(HashMap::from([(
            "session-1".to_string(),
            pending_approval::SessionLifecycleStatus::Running,
        )])),
        ..pending_approval::RecentSessionEvents::default()
    };

    assert_eq!(
        completion_state_response(&events, "session-1", "turn-completed"),
        json!({
            "status": "ok",
            "sessionId": "session-1",
            "turnId": "turn-completed",
            "sessionKnown": true,
            "turnKnown": true,
            "lifecycle": "running",
            "terminal": true,
            "terminalKind": "completed",
            "completedAt": 42,
        })
    );
    assert_eq!(
        completion_state_response(&events, "session-1", "turn-running")["terminal"],
        false
    );
    assert_eq!(
        completion_state_response(&events, "session-1", "turn-aborted")["terminalKind"],
        "aborted"
    );
    assert_eq!(
        completion_state_response(&events, "session-1", "turn-imported")["terminal"],
        false
    );
    let unknown = completion_state_response(&events, "session-1", "turn-missing");
    assert_eq!(unknown["turnKnown"], false);
    assert_eq!(unknown["terminal"], false);
}

#[tokio::test]
async fn renderer_api_keeps_notification_secrets_without_reveal_commands() {
    let state = Arc::new(AppState::default());
    state.config.write().await.prompt_optimization.api_key = "optimizer-secret".to_string();
    state
        .config
        .write()
        .await
        .webhook
        .channels
        .push(NotificationChannelConfig {
            id: "telegram-1".to_string(),
            kind: crate::notifications::NotificationChannelKind::Telegram,
            bot_token: "telegram-secret".to_string(),
            chat_id: "-100123".to_string(),
            ..NotificationChannelConfig::default()
        });

    let result = invoke_api(
        &state,
        "reveal_notification_channel",
        json!({ "channelId": "telegram-1" }),
    )
    .await;
    assert_eq!(result["status"], "failed");
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .contains("未知 Codey API 命令")
    );
    assert!(!result.to_string().contains("optimizer-secret"));
    assert!(!result.to_string().contains("telegram-secret"));
}

#[tokio::test]
async fn testing_an_incomplete_notification_draft_does_not_save_it() {
    let state = Arc::new(AppState::default());
    let before = state.config.read().await.clone();

    let result = invoke_api(
        &state,
        "test_notification_channel",
        json!({
            "channel": {
                "id": "incomplete-telegram",
                "kind": "telegram",
                "enabled": true,
                "botToken": "",
                "chatId": ""
            }
        }),
    )
    .await;

    assert_eq!(result["status"], "failed");
    assert_eq!(*state.config.read().await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_concurrent_config_saves_are_rejected_without_diverging_disk_and_memory() {
    let directory = tempfile::tempdir().unwrap();
    let initial = CodeyConfig::default();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let save_count = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(save_count + 1));
    let tasks = (0..save_count)
        .map(|index| {
            let state = Arc::clone(&state);
            let barrier = Arc::clone(&barrier);
            let mut input = initial.clone();
            input.user_scripts = vec![format!("// concurrent save {index}")];
            tokio::spawn(async move {
                barrier.wait().await;
                save_codey_config(&state, input).await
            })
        })
        .collect::<Vec<_>>();

    barrier.wait().await;
    let mut successes = 0;
    let mut conflicts = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => successes += 1,
            Err(error) => {
                assert!(error.contains("已被其他操作更新"));
                conflicts += 1;
            }
        }
    }

    assert_eq!(successes, 1);
    assert_eq!(conflicts, save_count - 1);
    let memory = state.config.read().await.clone();
    let disk = state.store.load().unwrap();
    assert_eq!(disk, memory);
    assert_eq!(memory.settings_revision, 1);
    assert_eq!(memory.user_scripts.len(), 1);
}

#[tokio::test]
async fn disabled_local_router_keeps_route_config_read_only_without_blocking_other_settings() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));
    let initial = CodeyConfig {
        local_router_enabled: false,
        ..CodeyConfig::default()
    };
    store.save(&initial).unwrap();
    let state = Arc::new(AppState {
        store,
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });

    let mut unrelated = initial;
    unrelated.slim_codex_pet = !unrelated.slim_codex_pet;
    save_codey_config_locked(&state, CodeyConfigSaveInput::complete(unrelated))
        .await
        .unwrap();
    let saved = state.config.read().await.clone();
    assert!(!saved.local_router_enabled);
    assert!(!saved.slim_codex_pet);
    assert_eq!(state.store.load().unwrap(), saved);

    let mut route_edit = saved.clone();
    route_edit.active_profile_id = "must-not-persist".to_string();
    let error =
        match save_codey_config_locked(&state, CodeyConfigSaveInput::complete(route_edit)).await {
            Ok(_) => panic!("read-only route edit unexpectedly succeeded"),
            Err(error) => error,
        };
    assert!(error.contains("只读"));
    assert_eq!(*state.config.read().await, saved);
    assert_eq!(state.store.load().unwrap(), saved);

    let mut reenabled = saved.clone();
    reenabled.local_router_enabled = true;
    save_codey_config_locked(&state, CodeyConfigSaveInput::complete(reenabled))
        .await
        .unwrap();
    let reenabled = state.config.read().await.clone();
    assert!(reenabled.local_router_enabled);
    assert_eq!(reenabled.profiles[0].name, saved.profiles[0].name);
    assert_eq!(state.store.load().unwrap(), reenabled);
}

#[tokio::test]
async fn disabled_local_router_skips_automatic_route_import_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let initial = CodeyConfig {
        local_router_enabled: false,
        ..CodeyConfig::default()
    };
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });

    assert!(!ensure_default_route_imported(&state).await);
    let error = mark_initial_route_import_completed(&state)
        .await
        .unwrap_err();
    assert!(error.contains("只读"));
    assert_eq!(*state.config.read().await, initial);
    assert!(!state.store.path().exists());
}

#[tokio::test]
async fn native_model_cache_without_saved_route_does_not_block_settings_or_reenable() {
    for provider_id in ["external-provider", "openai"] {
        let directory = tempfile::tempdir().unwrap();
        let mut initial = CodeyConfig {
            local_router_enabled: false,
            ..CodeyConfig::default()
        };
        initial.upstream_models_by_provider.insert(
            provider_id.into(),
            vec!["gpt-5.5".into(), "native-model".into()],
        );
        initial
            .selected_models_by_provider
            .insert(provider_id.into(), vec!["gpt-5.5".into()]);
        let initial = initial.normalize();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(initial.clone()),
            ..AppState::default()
        });
        let mut unrelated = initial.clone();
        unrelated.slim_codex_pet = !unrelated.slim_codex_pet;
        save_codey_config_locked(&state, CodeyConfigSaveInput::complete(unrelated))
            .await
            .unwrap();
        let saved = state.config.read().await.clone();
        assert_eq!(saved.profiles, initial.profiles);
        assert_eq!(
            saved.selected_models_by_provider,
            initial.selected_models_by_provider
        );
        assert_eq!(
            saved.upstream_models_by_provider,
            initial.upstream_models_by_provider
        );
        assert_eq!(state.store.load().unwrap(), saved);

        let mut reenabled = saved;
        reenabled.local_router_enabled = true;
        save_codey_config_locked(&state, CodeyConfigSaveInput::complete(reenabled))
            .await
            .unwrap();
        let reenabled = state.config.read().await.clone();
        assert!(reenabled.local_router_enabled);
        assert_eq!(reenabled.profiles, initial.profiles);
        assert_eq!(state.store.load().unwrap(), reenabled);
    }
}

#[tokio::test]
async fn legacy_save_without_local_router_field_preserves_disabled_state() {
    let directory = tempfile::tempdir().unwrap();
    let initial = CodeyConfig {
        local_router_enabled: false,
        ..CodeyConfig::default()
    };
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let mut payload = serde_json::to_value(initial).unwrap();
    payload
        .as_object_mut()
        .unwrap()
        .remove("localRouterEnabled");
    payload["slimCodexPet"] = json!(false);

    let result = invoke_api(&state, "save_codey_config", json!({ "config": payload })).await;

    assert_eq!(result["status"], "ok");
    let saved = state.config.read().await;
    assert!(!saved.local_router_enabled);
    assert!(!saved.slim_codex_pet);
}

#[tokio::test]
async fn legacy_save_without_subagent_roles_preserves_differentiated_roles() {
    let directory = tempfile::tempdir().unwrap();
    let mut initial = CodeyConfig::default();
    initial
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .model = "worker-specialized".to_string();
    initial
        .subagent_roles
        .get_mut("codey_deep_research")
        .unwrap()
        .reasoning_effort = "ultra".to_string();
    let expected_roles = initial.subagent_roles.clone();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let mut payload = serde_json::to_value(initial).unwrap();
    payload.as_object_mut().unwrap().remove("subagentRoles");
    payload["slimCodexPet"] = json!(false);

    let result = invoke_api(&state, "save_codey_config", json!({ "config": payload })).await;

    assert_eq!(result["status"], "ok");
    let saved = state.config.read().await;
    assert_eq!(saved.subagent_roles, expected_roles);
    assert!(!saved.slim_codex_pet);
}

#[tokio::test]
async fn legacy_subagent_scalars_update_only_the_default_role() {
    let directory = tempfile::tempdir().unwrap();
    let mut initial = CodeyConfig::default();
    initial
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .model = "worker-specialized".to_string();
    let expected_worker = initial.subagent_roles["codey_worker"].clone();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let mut payload = serde_json::to_value(initial).unwrap();
    let fields = payload.as_object_mut().unwrap();
    fields.remove("subagentRoles");
    fields.insert("subagentModel".to_string(), json!("legacy-default"));
    fields.insert("subagentReasoningEffort".to_string(), json!("high"));

    let result = invoke_api(&state, "save_codey_config", json!({ "config": payload })).await;

    assert_eq!(result["status"], "ok");
    let saved = state.config.read().await;
    assert_eq!(saved.subagent_roles["codey_worker"], expected_worker);
    assert_eq!(
        saved.subagent_roles[SUBAGENT_ROLE_DEFAULT].model,
        "legacy-default"
    );
    assert_eq!(
        saved.subagent_roles[SUBAGENT_ROLE_DEFAULT].reasoning_effort,
        "high"
    );
    assert_eq!(saved.subagent_model, "legacy-default");
    assert_eq!(saved.subagent_reasoning_effort, "high");
}

#[tokio::test]
async fn partial_subagent_role_payload_merges_with_existing_roles() {
    let directory = tempfile::tempdir().unwrap();
    let mut initial = CodeyConfig::default();
    initial
        .subagent_roles
        .get_mut("codey_deep_research")
        .unwrap()
        .model = "research-specialized".to_string();
    let expected_research = initial.subagent_roles["codey_deep_research"].clone();
    let expected_default = initial.subagent_roles[SUBAGENT_ROLE_DEFAULT].clone();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let mut payload = serde_json::to_value(initial).unwrap();
    payload["subagentRoles"] = json!({
        "codey_worker": {
            "model": "worker-updated",
            "reasoningEffort": "max"
        }
    });

    let result = invoke_api(&state, "save_codey_config", json!({ "config": payload })).await;

    assert_eq!(result["status"], "ok");
    let saved = state.config.read().await;
    assert_eq!(
        saved.subagent_roles["codey_deep_research"],
        expected_research
    );
    assert_eq!(
        saved.subagent_roles[SUBAGENT_ROLE_DEFAULT],
        expected_default
    );
    assert_eq!(saved.subagent_roles["codey_worker"].model, "worker-updated");
    assert_eq!(saved.subagent_roles["codey_worker"].reasoning_effort, "max");
}

#[tokio::test]
async fn custom_role_matrix_persists_official_models_for_the_current_provider() {
    let directory = tempfile::tempdir().unwrap();
    let initial = CodeyConfig::default();
    let provider_id = initial.current_provider_id().unwrap().to_string();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let mut payload = serde_json::to_value(initial).unwrap();
    payload["subagentRoles"] = json!({
        "codey_quick_scan": {
            "model": "gpt-5.6-luna",
            "reasoningEffort": "low"
        },
        "codey_deep_research": {
            "model": "gpt-5.6-luna",
            "reasoningEffort": "high"
        },
        "codey_visual_analysis": {
            "model": "gpt-5.6-terra",
            "reasoningEffort": "high"
        },
        "codey_worker": {
            "model": "gpt-5.6-terra",
            "reasoningEffort": "max"
        },
        "codey_visual_worker": {
            "model": "gpt-5.6-terra",
            "reasoningEffort": "max"
        },
        "default": {
            "model": "gpt-5.6-terra",
            "reasoningEffort": "low"
        }
    });

    let result = invoke_api(&state, "save_codey_config", json!({ "config": payload })).await;

    assert_eq!(result["status"], "ok");
    let saved = state.config.read().await.clone();
    assert_eq!(
        saved.subagent_roles["codey_quick_scan"],
        SubagentRoleConfig::new(
            crate::local_router::model_alias(&provider_id, "gpt-5.6-luna"),
            "low",
        )
    );
    assert_eq!(
        saved.subagent_roles["codey_worker"],
        SubagentRoleConfig::new(
            crate::local_router::model_alias(&provider_id, "gpt-5.6-terra"),
            "max",
        )
    );
    assert_eq!(
        saved.declared_official_models_by_provider[&provider_id],
        ["gpt-5.6-luna", "gpt-5.6-terra"]
    );
    assert_eq!(
        saved.upstream_models_by_provider[&provider_id],
        ["gpt-5.6-luna", "gpt-5.6-terra"]
    );
    assert!(!saved.selected_models_by_provider.contains_key(&provider_id));
    assert!(
        !saved
            .manual_third_party_models_by_provider
            .contains_key(&provider_id)
    );
    assert_eq!(state.store.load().unwrap(), saved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_join_does_not_hold_the_config_write_lock() {
    let directory = tempfile::tempdir().unwrap();
    let initial = CodeyConfig::default();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (shutdown_seen_tx, shutdown_seen_rx) = oneshot::channel();
    let release = Arc::new(Notify::new());
    let watcher_release = Arc::clone(&release);
    let watcher_task = tokio::spawn(async move {
        let _ = shutdown_rx.await;
        let _ = shutdown_seen_tx.send(());
        watcher_release.notified().await;
    });
    *state.waiting_watcher_shutdown.lock().await = Some(shutdown_tx);
    *state.waiting_watcher_task.lock().await = Some(watcher_task);

    let mut input = initial;
    input.slim_codex_pet = !input.slim_codex_pet;
    let save_state = Arc::clone(&state);
    let save_task = tokio::spawn(async move { save_codey_config(&save_state, input).await });
    tokio::time::timeout(Duration::from_secs(1), shutdown_seen_rx)
        .await
        .expect("watcher shutdown should start")
        .unwrap();

    let config_guard =
        tokio::time::timeout(Duration::from_millis(100), state.config_write_lock.lock())
            .await
            .expect("watcher join must happen after releasing the config write lock");
    drop(config_guard);
    release.notify_one();
    save_task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_sync_does_not_block_config_writes_or_commit_a_stale_result() {
    let directory = tempfile::tempdir().unwrap();
    let initial = CodeyConfig::default();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(initial.clone()),
        ..AppState::default()
    });
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let sync_state = Arc::clone(&state);
    let sync_task = tokio::spawn(async move {
        sync_provider_state_with(&sync_state, move |mut config| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            config.profiles[0].name = "stale provider".to_string();
            let mut status = codex_provider::status_from_config(&config);
            status.changed = true;
            Ok((config, status))
        })
        .await
    });
    started_rx.await.unwrap();

    let mut settings = initial;
    settings.slim_codex_pet = !settings.slim_codex_pet;
    let config_guard =
        tokio::time::timeout(Duration::from_millis(500), state.config_write_lock.lock())
            .await
            .expect("provider inspection must not hold the config write lock");
    save_codey_config_locked(&state, CodeyConfigSaveInput::complete(settings))
        .await
        .unwrap();
    drop(config_guard);
    release_tx.send(()).unwrap();

    let error = sync_task.await.unwrap().unwrap_err();
    assert!(error.contains("已忽略过期"));
    let memory = state.config.read().await.clone();
    let disk = state.store.load().unwrap();
    assert_eq!(disk, memory);
    assert_ne!(memory.profiles[0].name, "stale provider");
    assert_eq!(memory.settings_revision, 1);
}

#[test]
fn model_sync_can_defer_catalog_refresh_until_a_model_is_selectable() {
    assert!(!should_refresh_model_catalog(
        &model_catalog::ModelSelectionState::default()
    ));

    let mut state = model_catalog::ModelSelectionState::default();
    state.third_party_models.push("provider-model".to_string());
    assert!(should_refresh_model_catalog(&state));
}

#[cfg(windows)]
#[test]
fn selected_codex_app_path_requires_a_desktop_executable() {
    let directory = tempfile::tempdir().unwrap();
    assert!(validate_codex_app_path(directory.path().to_str().unwrap()).is_err());

    let executable = directory.path().join("Codex.exe");
    fs::write(&executable, []).unwrap();
    assert_eq!(
        validate_codex_app_path(directory.path().to_str().unwrap()).unwrap(),
        directory.path()
    );
}

#[cfg(windows)]
#[test]
fn selected_codex_app_path_accepts_a_custom_install_root() {
    let directory = tempfile::tempdir().unwrap();
    let install_root = directory.path().join("D drive").join("OpenAI Codex");
    let current = install_root.join("versions").join("current");
    fs::create_dir_all(&current).unwrap();
    fs::write(current.join("ChatGPT.exe"), []).unwrap();

    assert_eq!(
        validate_codex_app_path(install_root.to_str().unwrap()).unwrap(),
        current
    );
}

#[test]
fn update_manifest_reports_a_newer_https_release() {
    let manifest = serde_json::from_value::<UpdateManifest>(json!({
        "schema_version": 1,
        "version": "0.2.0",
        "tag": "v0.2.0",
        "assets": [{
            "platform": "windows",
            "arch": "x64",
            "package_type": "nsis",
            "file_name": "Codey-0.2.0-windows-x64-setup.exe",
            "url": "https://updates.example.com/releases/v0.2.0/Codey-0.2.0-windows-x64-setup.exe",
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "size": 1024
        }]
    }))
    .unwrap();

    let result = assess_update_manifest("0.1.0", &manifest).unwrap();

    assert_eq!(result.current_version, "0.1.0");
    assert_eq!(result.latest_version, "0.2.0");
    assert!(result.update_available);
}

#[test]
fn update_manifest_selects_only_a_supported_current_platform_installer() {
    let platform = current_update_platform();
    let arch = current_update_arch();
    let (package_type, file_name, expected_package_type) = match platform {
        "windows" => (
            "nsis",
            format!("Codey-0.2.0-windows-{arch}-setup.exe"),
            Some("nsis"),
        ),
        "macos" => (
            "app-zip",
            format!("Codey-0.2.0-macos-{arch}-unsigned.zip"),
            Some("app-zip"),
        ),
        _ => (
            "app-zip",
            format!("Codey-0.2.0-{platform}-{arch}-unsupported.zip"),
            None,
        ),
    };
    let manifest = serde_json::from_value::<UpdateManifest>(json!({
        "schema_version": 1,
        "version": "0.2.0",
        "tag": "v0.2.0",
        "assets": [{
            "platform": platform,
            "arch": arch,
            "package_type": package_type,
            "file_name": &file_name,
            "url": format!("https://updates.example.com/releases/v0.2.0/{file_name}"),
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "size": 2048
        }]
    }))
    .unwrap();

    let result = assess_update_manifest("0.1.0", &manifest).unwrap();

    assert_eq!(
        result
            .selected_asset
            .as_ref()
            .map(|asset| asset.package_type.as_str()),
        expected_package_type
    );
    assert_eq!(
        result
            .selected_asset
            .as_ref()
            .map(|asset| asset.arch.as_str()),
        expected_package_type.map(|_| arch)
    );
    assert_eq!(
        result
            .selected_asset
            .as_ref()
            .map(|asset| asset.file_name.as_str()),
        expected_package_type.map(|_| file_name.as_str())
    );
}

#[tokio::test]
async fn app_state_preserves_update_shutdown_reason() {
    let state = AppState::default();

    state.request_update_shutdown();
    state.request_shutdown();

    assert_eq!(
        state.wait_for_shutdown().await,
        AppShutdownReason::InstallUpdate
    );
}

#[tokio::test]
async fn shutdown_signal_wakes_every_waiter_without_losing_the_reason() {
    let state = Arc::new(AppState::default());
    let waiters = (0..8)
        .map(|_| {
            let state = state.clone();
            tokio::spawn(async move { state.wait_for_shutdown().await })
        })
        .collect::<Vec<_>>();
    tokio::task::yield_now().await;

    state.request_update_shutdown();

    for waiter in waiters {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("shutdown waiter timed out")
                .expect("shutdown waiter panicked"),
            AppShutdownReason::InstallUpdate
        );
    }
}

#[test]
fn update_manifest_rejects_insecure_asset_urls() {
    let manifest = serde_json::from_value::<UpdateManifest>(json!({
        "schema_version": 1,
        "version": "0.2.0",
        "tag": "v0.2.0",
        "assets": [{
            "platform": "windows",
            "arch": "x64",
            "package_type": "nsis",
            "file_name": "Codey-0.2.0-windows-x64-setup.exe",
            "url": "http://updates.example.com/Codey-0.2.0-windows-x64-setup.exe",
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "size": 1024
        }]
    }))
    .unwrap();

    assert!(
        assess_update_manifest("0.1.0", &manifest)
            .unwrap_err()
            .contains("必须使用 HTTPS")
    );
}

#[test]
fn update_manifest_rejects_asset_path_traversal() {
    let manifest = serde_json::from_value::<UpdateManifest>(json!({
        "schema_version": 1,
        "version": "0.2.0",
        "tag": "v0.2.0",
        "assets": [{
            "platform": "windows",
            "arch": "x64",
            "package_type": "nsis",
            "file_name": "../Codey.exe",
            "url": "https://updates.example.com/Codey.exe",
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "size": 1024
        }]
    }))
    .unwrap();

    assert!(
        assess_update_manifest("0.1.0", &manifest)
            .unwrap_err()
            .contains("文件名无效")
    );
}
