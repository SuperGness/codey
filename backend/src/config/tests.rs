use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{local_router, model_catalog};

use super::*;

fn named_config(name: &str) -> CodeyConfig {
    let mut config = CodeyConfig::default();
    config.profiles[0].name = name.to_string();
    config
}

#[test]
fn official_concurrency_profile_migrates_and_rejects_invalid_values() {
    let mut value = serde_json::to_value(ProviderProfile::new("测试线路")).unwrap();
    value.as_object_mut().unwrap().remove("maxConcurrency");
    let legacy: ProviderProfile = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(legacy.max_concurrency, 0);
    for limit in [0, 1, u32::MAX] {
        value["maxConcurrency"] = serde_json::json!(limit);
        let profile: ProviderProfile = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(profile.max_concurrency, limit);
    }
    for invalid in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(u64::from(u32::MAX) + 1),
        serde_json::json!("3"),
        serde_json::json!(true),
        serde_json::Value::Null,
    ] {
        value["maxConcurrency"] = invalid;
        assert!(serde_json::from_value::<ProviderProfile>(value.clone()).is_err());
    }
}

#[test]
fn official_concurrency_derivation_keeps_legacy_and_account_settings_separate() {
    let mut legacy = ProviderProfile::new("兼容官方线路");
    legacy.official_account = true;
    legacy.max_concurrency = 3;
    let mut config = CodeyConfig {
        profiles: vec![legacy.clone()],
        ..CodeyConfig::default()
    };
    let mut derived = legacy.clone();
    derived.max_concurrency = 0;
    config.apply_launch_official_profiles(vec![derived.clone()]);
    assert_eq!(config.profiles[0].max_concurrency, 3);
    derived.official_account_id = Some("acct-limit".into());
    derived.max_concurrency = 8;
    config.apply_launch_official_profiles(vec![derived.clone()]);
    assert_eq!(config.profiles[0].max_concurrency, 8);
    derived.max_concurrency = 0;
    config.apply_launch_official_profiles(vec![derived]);
    assert_eq!(config.profiles[0].max_concurrency, 0);
}

#[test]
fn outbound_api_urls_allow_http_or_https() {
    for accepted in [
        "https://api.example.com/v1",
        "http://localhost:11434/v1",
        "http://127.0.0.2:8080/v1",
        "http://[::1]:8080/v1",
        "http://api.example.com/v1",
        "http://192.168.1.8:8080/v1",
    ] {
        assert!(
            validate_outbound_api_url(accepted, "测试 API 地址").is_ok(),
            "{accepted}"
        );
    }

    for rejected in [
        "ftp://localhost/models",
        "https://user:password@api.example.com/v1",
        "http://token@localhost:11434/v1",
        "http://0.0.0.0:8080/v1",
        "http://169.254.169.254/latest/meta-data",
        "http://[::]:8080/v1",
        "http://[fe80::1]:8080/v1",
    ] {
        assert!(
            validate_outbound_api_url(rejected, "测试 API 地址").is_err(),
            "{rejected}"
        );
    }
}

#[test]
fn upstream_proxy_urls_validate_scheme_and_host() {
    for accepted in [
        "http://127.0.0.1:7890",
        "https://proxy.example.com:8443",
        "socks5://127.0.0.1:1080",
        "socks5h://proxy.example.com:1080",
        // 代理认证凭据允许写在地址里，区别于 API URL。
        "http://user:pass@proxy.example.com:8080",
    ] {
        assert!(
            validate_outbound_proxy_url(accepted, "测试代理").is_ok(),
            "{accepted}"
        );
    }
    for rejected in [
        "ftp://proxy.example.com:21",
        "socks4://127.0.0.1:1080",
        "127.0.0.1:7890",
        "http://0.0.0.0:7890",
        "http://[fe80::1]:7890",
    ] {
        assert!(
            validate_outbound_proxy_url(rejected, "测试代理").is_err(),
            "{rejected}"
        );
    }

    let mut config = named_config("代理线路");
    config.profiles[0].base_url = "https://api.example.com/v1".to_string();
    config.profiles[0].api_key = "sk-test".to_string();
    config.profiles[0].upstream_proxy = "ftp://proxy.example.com".to_string();
    assert!(
        config.profiles[0]
            .validate()
            .unwrap_err()
            .contains("上游代理")
    );
    config.profiles[0].upstream_proxy = "socks5://127.0.0.1:1080".to_string();
    assert!(config.profiles[0].validate().is_ok());
}

#[test]
fn codex_launch_proxy_rejects_paths_and_portless_socks_but_allows_credentials() {
    for accepted in [
        "http://127.0.0.1:7890",
        "https://proxy.example.com:8443",
        "https://proxy.example.com",
        "socks5://127.0.0.1:1080",
        "socks5h://proxy.example.com:1080",
        "http://[::1]:7890",
        // 凭据交给本地认证中继，不交给 Chromium 命令行。
        "http://user:pass@proxy.example.com:8080",
        "socks5://user@127.0.0.1:1080",
    ] {
        assert!(validate_codex_launch_proxy(accepted).is_ok(), "{accepted}");
    }
    for rejected in [
        // 只允许协议、地址、端口和认证信息；非特殊协议的 path 解析为空串。
        "http://127.0.0.1:7890/v1",
        "socks5://127.0.0.1:1080/v1",
        "http://127.0.0.1:7890?x=1",
        "http://127.0.0.1:7890#frag",
        // Chromium 需要 host:port，SOCKS 不能省略端口。
        "socks5://127.0.0.1",
        "socks5h://proxy.example.com",
        // RFC 1929 的用户名和密码各限 255 字节。
        &format!("socks5://{}@127.0.0.1:1080", "u".repeat(256)),
        // 通用约束沿用线路上游代理。
        "ftp://proxy.example.com:21",
        "127.0.0.1:7890",
        "http://0.0.0.0:7890",
    ] {
        assert!(validate_codex_launch_proxy(rejected).is_err(), "{rejected}");
    }
}

#[test]
fn enabled_prompt_optimization_requires_complete_connection_settings() {
    let mut optimization = PromptOptimizationConfig {
        enabled: true,
        ..PromptOptimizationConfig::default()
    };
    assert!(optimization.validate().unwrap_err().contains("API 地址"));

    optimization.base_url = "https://api.example.com/v1".to_string();
    assert!(optimization.validate().unwrap_err().contains("API Key"));

    optimization.api_key = "sk-test".to_string();
    assert!(optimization.validate().unwrap_err().contains("模型"));

    optimization.model = "gpt-test".to_string();
    assert!(optimization.validate().is_ok());

    optimization.mode = PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE.to_string();
    optimization.base_url.clear();
    optimization.api_key.clear();
    assert!(optimization.validate().is_ok());
}

#[test]
fn provider_profiles_cannot_shadow_the_internal_router_provider() {
    let mut profile = ProviderProfile::new("Reserved route");
    profile.id = local_router::ROUTER_PROVIDER_ID.to_string();
    profile.base_url = "https://relay.example/v1".into();
    profile.api_key = "sk-test".into();
    profile.normalize();

    assert!(
        profile
            .validate()
            .unwrap_err()
            .contains("Codey 内部 Provider ID")
    );
}

#[cfg(unix)]
#[test]
fn config_temp_files_are_private_before_atomic_replace() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));

    store.save(&named_config("private")).unwrap();

    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn config_save_does_not_leave_a_plaintext_temp_file() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));

    store.save(&CodeyConfig::default()).unwrap();

    let names = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, [std::ffi::OsString::from("config.json")]);
}

#[test]
fn old_config_gains_alias_history_and_preserves_model_on_route_identity_change() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));
    let mut route = ProviderProfile::new("Old route");
    route.id = "old/route".into();
    route.base_url = "https://relay.example/v1".into();
    let config = CodeyConfig {
        profiles: vec![route],
        selected_models_by_provider: BTreeMap::from([(
            "old/route".into(),
            vec!["vendor/model".into()],
        )]),
        default_model: "old%2Froute/vendor/model".into(),
        subagent_roles: uniform_subagent_roles("old%2Froute/vendor/model", "high"),
        ..CodeyConfig::default()
    };
    let mut old_json = serde_json::to_value(config).unwrap();
    old_json
        .as_object_mut()
        .unwrap()
        .remove("modelAliasHistory");
    fs::write(store.path(), serde_json::to_vec(&old_json).unwrap()).unwrap();
    let mut loaded = store.load().unwrap();
    assert_eq!(
        loaded.model_alias_history["old%2froute/vendor/model"],
        "vendor/model"
    );
    loaded.profiles[0].id = "new-route".into();
    loaded.selected_models_by_provider = BTreeMap::from([(
        "new-route".into(),
        vec!["other-model".into(), "vendor/model".into()],
    )]);
    loaded = loaded.normalize();
    assert_eq!(loaded.default_model, "new-route/vendor/model");
    assert_eq!(loaded.subagent_model, "new-route/vendor/model");
    loaded.local_router_enabled = false;
    store.save(&loaded).unwrap();
    let restored = store.load().unwrap();
    assert_eq!(
        restored.model_alias_history["old%2froute/vendor/model"],
        "vendor/model"
    );
    assert_eq!(restored.clone().normalize(), restored);
}

#[test]
fn config_load_recovers_from_the_newest_valid_backup() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));

    store.save(&named_config("version-1")).unwrap();
    store.save(&named_config("version-2")).unwrap();
    fs::write(store.path(), b"{broken-json").unwrap();

    let recovered = store.load().unwrap();
    assert_eq!(recovered.profiles[0].name, "version-1");
}

#[test]
fn config_load_skips_a_corrupt_newer_backup() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));

    for version in 1..=4 {
        store
            .save(&named_config(&format!("version-{version}")))
            .unwrap();
    }
    fs::write(store.path(), b"corrupt-primary").unwrap();
    fs::write(store.backup_path(1), b"corrupt-backup").unwrap();

    let recovered = store.load().unwrap();
    assert_eq!(recovered.profiles[0].name, "version-2");
}

// 【自动化测试】配置存储 - 无法读取的配置另存到轮转之外，保存默认值后仍可找回
#[test]
fn unreadable_configs_are_preserved_outside_the_backup_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));
    fs::write(store.path(), b"corrupt-primary").unwrap();
    fs::write(store.backup_path(1), b"corrupt-backup").unwrap();
    assert!(store.load().is_err());

    let preserved = store.preserve_unreadable();
    assert_eq!(preserved.len(), 2);
    assert_eq!(fs::read(&preserved[0]).unwrap(), b"corrupt-primary");
    assert_eq!(fs::read(&preserved[1]).unwrap(), b"corrupt-backup");
    assert_eq!(store.preserve_unreadable(), preserved);

    for version in 1..=4 {
        store
            .save(&named_config(&format!("version-{version}")))
            .unwrap();
    }
    assert_eq!(fs::read(&preserved[0]).unwrap(), b"corrupt-primary");
    assert_eq!(fs::read(&preserved[1]).unwrap(), b"corrupt-backup");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 6);
}

#[cfg(unix)]
#[test]
fn config_backups_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(directory.path().join("config.json"));
    store.save(&named_config("version-1")).unwrap();
    store.save(&named_config("version-2")).unwrap();

    assert_eq!(
        fs::metadata(store.backup_path(1))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn provider_request_headers_are_serialized_for_route_editing() {
    let mut profile = ProviderProfile::new("Private Relay");
    profile
        .model_request_headers
        .insert("Authorization".to_string(), "secret".to_string());

    let serialized = serde_json::to_value(profile).unwrap();

    assert_eq!(
        serialized
            .get("modelRequestHeaders")
            .and_then(|headers| headers.get("Authorization"))
            .and_then(serde_json::Value::as_str),
        Some("secret")
    );
}

#[test]
fn deprecated_provider_protocol_fields_are_ignored() {
    let profile = serde_json::from_value::<ProviderProfile>(serde_json::json!({
        "id": "legacy-provider",
        "name": "Legacy Provider",
        "baseUrl": "https://gateway.example/v1",
        "apiKey": "",
        "protocol": "chatCompletions",
        "chatCompletionsModels": ["legacy-model"]
    }))
    .unwrap();

    let serialized = serde_json::to_value(profile).unwrap();

    assert!(serialized.get("protocol").is_none());
    assert!(serialized.get("chatCompletionsModels").is_none());
}

#[test]
fn legacy_official_route_without_auth_mode_is_normalized_on_load() {
    let legacy = serde_json::json!({
        "activeProfileId": "codey_global",
        "profiles": [{
            "id": "codey_global",
            "name": "OpenAI 官方直登",
            "baseUrl": "https://chatgpt.com/backend-api/codex",
            "apiKey": ""
        }]
    });
    let loaded = parse_config_contents(&legacy.to_string(), Path::new("config.json")).unwrap();
    let profile = &loaded.profiles[0];
    assert!(profile.official_account);
    assert_eq!(profile.auth_mode, AUTH_MODE_OFFICIAL_ACCOUNT);
    assert_eq!(profile.upstream_protocol, UPSTREAM_PROTOCOL_OFFICIAL);
    assert_eq!(profile.short_name, "Op");
    assert_eq!(profile.display_short_name(), "Op");
    assert!(profile.validate().is_ok());
    assert_eq!(loaded.clone().normalize(), loaded);

    for (field, value) in [
        ("authMode", serde_json::json!(AUTH_MODE_API_KEY)),
        ("apiKey", serde_json::json!("sk-test")),
        ("apiKeyConfigured", serde_json::json!(true)),
        ("baseUrl", serde_json::json!("https://api.openai.com/v1")),
        (
            "baseUrl",
            serde_json::json!("https://chatgpt.com.relay.example/backend-api/codex"),
        ),
        (
            "baseUrl",
            serde_json::json!("http://chatgpt.com/backend-api/codex"),
        ),
    ] {
        let mut config = legacy.clone();
        config["profiles"][0][field] = value;
        let loaded = parse_config_contents(&config.to_string(), Path::new("config.json")).unwrap();
        assert!(!loaded.profiles[0].official_account, "{field}");
    }
}

#[test]
fn third_party_websocket_support_defaults_off_and_only_allows_responses() {
    let legacy = serde_json::from_value::<ProviderProfile>(serde_json::json!({
        "id": "legacy-provider",
        "name": "Legacy Provider",
        "shortName": "旧",
        "baseUrl": "https://gateway.example/v1",
        "apiKey": "sk-test",
        "upstreamProtocol": "openaiResponses"
    }))
    .unwrap();
    assert!(!legacy.supports_websockets);
    assert!(!legacy.supports_native_web_search);
    assert!(!legacy.supports_auto_review);

    let mut chat = ProviderProfile::new("Chat Relay");
    chat.base_url = "https://gateway.example/v1".into();
    chat.api_key = "sk-test".into();
    chat.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    chat.supports_websockets = true;
    chat.normalize();
    assert!(
        chat.validate()
            .unwrap_err()
            .contains("只有 OpenAI Responses")
    );

    let mut responses = ProviderProfile::new("Responses Relay");
    responses.base_url = "https://gateway.example/v1".into();
    responses.api_key = "sk-test".into();
    responses.supports_websockets = true;
    responses.normalize();
    assert!(responses.validate().is_ok());

    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    assert!(official.official_account);
    assert!(official.supports_websockets);
    assert!(official.supports_native_web_search);
    assert!(official.supports_auto_review);
    assert_eq!(official.upstream_protocol, UPSTREAM_PROTOCOL_OFFICIAL);
    assert!(official.validate().is_ok());
}

#[test]
fn auto_review_is_filtered_from_regular_model_state() {
    let mut config = CodeyConfig::default();
    let provider_id = config.current_provider_id().unwrap().to_string();
    let models = vec![
        "provider-model".to_string(),
        local_router::CODEX_AUTO_REVIEW_MODEL.to_string(),
    ];
    config
        .selected_models_by_provider
        .insert(provider_id.clone(), models.clone());
    config
        .manual_third_party_models_by_provider
        .insert(provider_id.clone(), models.clone());
    config
        .upstream_models_by_provider
        .insert(provider_id.clone(), models);

    let normalized = config.normalize();

    assert_eq!(
        normalized.enabled_route_models(&provider_id),
        ["provider-model"]
    );
    assert_eq!(
        normalized.upstream_models_by_provider[&provider_id],
        ["provider-model"]
    );
    assert!(!normalized.profiles[0].supports_auto_review);
}

#[test]
fn chat_completions_protocol_still_exposes_responses_to_codex() {
    let mut profile = ProviderProfile::new("Chat Relay");
    profile.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string();
    profile.normalize();

    assert_eq!(
        profile.upstream_protocol,
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
    );
    assert_eq!(profile.runtime_wire_api().unwrap(), "responses");
}

#[test]
fn anthropic_messages_protocol_still_exposes_responses_to_codex() {
    let mut profile = ProviderProfile::new("Anthropic");
    profile.upstream_protocol = UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.to_string();
    profile.normalize();

    assert_eq!(
        profile.upstream_protocol,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
    );
    assert_eq!(profile.runtime_wire_api().unwrap(), "responses");
}

#[test]
fn redacted_third_party_route_requires_its_saved_secret_before_validation() {
    let mut saved = ProviderProfile::new("Relay");
    saved.id = "relay".into();
    saved.base_url = "https://relay.example/v1".into();
    saved.api_key = "secret-token".into();
    saved.normalize();

    let mut redacted = saved.clone();
    redacted.api_key.clear();
    redacted.api_key_configured = true;
    assert!(redacted.validate().is_err());

    redacted.merge_redacted_secret(Some(&saved));
    redacted.normalize();
    assert!(redacted.validate().is_ok());
    assert_eq!(redacted.api_key, "secret-token");
}

#[test]
fn legacy_short_names_are_migrated_and_custom_names_stay_limited() {
    let mut route = ProviderProfile::new("中转线路");
    route.id = "relay".into();
    route.base_url = "https://relay.example/v1".into();
    route.api_key = "relay-key".into();

    route.short_name.clear();
    route.normalize();
    assert!(route.validate().is_ok());
    assert_eq!(route.display_short_name(), "中转");

    route.short_name = "中转线路".into();
    assert!(route.validate().is_ok());
    route.short_name = "中转线路名".into();
    assert!(route.validate().unwrap_err().contains("最多 4 个字符"));

    route.short_name = OFFICIAL_ROUTE_SHORT_NAME.into();
    assert!(route.validate().unwrap_err().contains("官方账号专属"));

    route.short_name.clear();
    let migrated = CodeyConfig {
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        initial_route_import_completed: true,
        ..CodeyConfig::default()
    }
    .normalize();
    assert_eq!(migrated.profiles[0].short_name, "中转");
    assert_eq!(migrated.profiles[0].display_short_name(), "中转");
    assert!(migrated.profiles[0].validate().is_ok());
}

#[test]
fn legacy_empty_short_names_receive_unique_defaults() {
    let mut first = ProviderProfile::new("线路 A");
    first.id = "route-a".into();
    first.short_name.clear();
    first.base_url = "https://a.example/v1".into();
    first.api_key = "a-key".into();
    let mut second = ProviderProfile::new("线路 B");
    second.id = "route-b".into();
    second.short_name.clear();
    second.base_url = "https://b.example/v1".into();
    second.api_key = "b-key".into();

    let migrated = CodeyConfig {
        active_profile_id: first.id.clone(),
        profiles: vec![first, second],
        initial_route_import_completed: true,
        ..CodeyConfig::default()
    }
    .normalize();

    assert_eq!(migrated.profiles[0].short_name, "线路");
    assert_eq!(migrated.profiles[1].short_name, "线1");
    assert_eq!(migrated.profiles[0].display_short_name(), "线路");
    assert_eq!(migrated.profiles[1].display_short_name(), "线1");
    assert!(validate_provider_profiles(&migrated.profiles).is_ok());
}

#[test]
fn loading_legacy_protocol_keeps_read_compatibility_without_rewriting_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let mut config = CodeyConfig::default();
    config.profiles[0].upstream_protocol = "old-unrecognized-protocol".into();
    let original = serde_json::to_vec(&config).unwrap();
    std::fs::write(&path, &original).unwrap();
    let loaded = ConfigStore::new(&path).load().unwrap();
    assert_eq!(
        loaded.profiles[0].upstream_protocol,
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn route_validation_rejects_duplicate_short_names() {
    let mut first = ProviderProfile::new("First");
    first.id = "first".into();
    first.short_name = "同".into();
    first.base_url = "https://first.example/v1".into();
    first.api_key = "first-key".into();
    first.normalize();
    let mut second = ProviderProfile::new("Second");
    second.id = "second".into();
    second.short_name = "同".into();
    second.base_url = "https://second.example/v1".into();
    second.api_key = "second-key".into();
    second.normalize();

    let error = validate_provider_profiles(&[first, second]).unwrap_err();
    assert!(error.contains("相同的短名称：同"));
}

#[test]
fn short_name_only_routes_preserve_blank_names_through_serialization() {
    for official in [false, true] {
        let mut route = ProviderProfile::new("");
        route.id = "route-id".into();
        route.short_name = "主".into();
        route.base_url = "https://relay.example/v1".into();
        route.api_key = "relay-key".into();
        if official {
            route.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        }
        route.normalize();
        assert!(route.validate().is_ok());
        let config = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            ..CodeyConfig::default()
        }
        .normalize();
        let saved = serde_json::to_string(&config).unwrap();
        let reloaded = serde_json::from_str::<CodeyConfig>(&saved)
            .unwrap()
            .normalize();
        assert_eq!(reloaded.profiles[0].name, "");
        assert_eq!(reloaded.profiles[0].short_name, "主");
        assert_eq!(reloaded.profiles[0].provider_id(), "route-id");
    }
}

#[test]
fn official_routes_keep_a_custom_short_name_and_fall_back_to_the_name() {
    let mut route = ProviderProfile::new("Official");
    route.short_name = "自定".into();
    route.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    route.normalize();

    assert_eq!(route.short_name, "自定");
    assert!(route.validate().is_ok());

    route.short_name.clear();
    route.normalize();
    assert!(route.short_name.is_empty());
    assert_eq!(route.display_short_name(), "Of");
    route.name = "Plus".into();
    assert_eq!(route.display_short_name(), "Pl");
    route.name = "😀备用".into();
    assert_eq!(route.display_short_name(), "😀备");
    assert!(route.validate().is_ok());

    route.short_name = "官方线路".into();
    assert!(route.validate().is_ok());
    route.short_name = "官方线路名".into();
    route.normalize();
    assert!(route.validate().unwrap_err().contains("最多 4 个字符"));
}

#[test]
fn generated_official_account_names_follow_the_add_order() {
    assert_eq!(default_official_route_name(1), "官方账号1");
    assert_eq!(default_official_route_name(10), "官方账号10");

    let mut route = ProviderProfile::new(default_official_route_name(1));
    route.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    route.normalize();
    assert_eq!(route.name, "官方账号1");
    assert!(route.short_name.is_empty());
    assert_eq!(route.display_short_name(), "官方");
    assert!(route.validate().is_ok());
}

#[test]
fn official_short_names_are_unique_against_third_party_routes() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.short_name = "官".into();
    official.normalize();
    official.short_name = "主".into();

    let mut relay = ProviderProfile::new("Relay");
    relay.id = "relay".into();
    relay.short_name = "主".into();
    relay.base_url = "https://relay.example/v1".into();
    relay.api_key = "relay-key".into();
    relay.normalize();

    let error = validate_provider_profiles(&[official, relay]).unwrap_err();
    assert!(error.contains("相同的短名称：主"));
}

#[test]
fn route_validation_rejects_duplicate_runtime_provider_ids() {
    let mut first = ProviderProfile::new("First");
    first.id = "first".into();
    first.base_url = "https://first.example/v1".into();
    first.api_key = "first-key".into();
    first.source_provider_id = Some("shared-provider".into());
    first.normalize();

    let mut second = ProviderProfile::new("Second");
    second.id = "second".into();
    second.base_url = "https://second.example/v1".into();
    second.api_key = "second-key".into();
    second.source_provider_id = Some("shared-provider".into());
    second.normalize();

    let error = validate_provider_profiles(&[first, second]).unwrap_err();
    assert!(error.contains("shared-provider"));
}

#[test]
fn runtime_catalog_combines_models_from_every_registered_route() {
    let mut official = ProviderProfile::new("Official");
    official.id = "official-profile".into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();

    let mut relay = ProviderProfile::new("Relay");
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
    config.upstream_models_by_provider.insert(
        "relay".into(),
        vec!["relay-a".into(), "shared-model".into()],
    );
    config.selected_models_by_provider.insert(
        "relay".into(),
        vec!["shared-model".into(), "manual-model".into()],
    );

    let (upstream, selected) = config.runtime_catalog_models();

    assert!(upstream.iter().any(|model| model == "gpt-5.6-sol"));
    assert!(upstream.iter().any(|model| model == "relay/relay-a"));
    assert!(upstream.iter().any(|model| model == "relay/manual-model"));
    assert_eq!(
        upstream
            .iter()
            .filter(|model| model.as_str() == "relay/shared-model")
            .count(),
        1
    );
    assert_eq!(
        selected,
        [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "relay/shared-model",
            "relay/manual-model",
        ]
    );
}

#[test]
fn runtime_catalog_display_names_use_route_short_names_for_aliased_ids() {
    let mut first = ProviderProfile::new("Official");
    first.id = "codey-official-account-one".into();
    first.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    first.official_account_id = Some("acct-one".into());
    first.short_name = "main".into();
    first.normalize();

    let mut second = ProviderProfile::new("Official Two");
    second.id = "codey-official-account-two".into();
    second.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    second.official_account_id = Some("acct-two".into());
    second.short_name = "alt".into();
    second.normalize();

    let mut relay = ProviderProfile::new("Relay");
    relay.id = "relay".into();
    relay.short_name = "pro".into();
    relay.base_url = "https://relay.example/v1".into();
    relay.api_key = "relay-key".into();
    relay.normalize();

    let mut config = CodeyConfig {
        active_profile_id: first.id.clone(),
        profiles: vec![first, second, relay],
        local_router_enabled: true,
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    };
    config
        .selected_models_by_provider
        .insert("relay".into(), vec!["shared-model".into()]);

    let names = config.runtime_catalog_display_names();

    assert_eq!(
        names.get("relay/shared-model").map(String::as_str),
        Some("[pro] shared-model")
    );
    assert_eq!(
        names
            .get("codey-official-account-one/gpt-5.6-sol")
            .map(String::as_str),
        Some("[main] gpt-5.6-sol")
    );
}

#[test]
fn normalize_drops_retired_official_models_from_saved_selections() {
    let mut official = ProviderProfile::new("Official");
    official.id = "official-profile".into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();

    let mut retired_only = ProviderProfile::new("Official Second");
    retired_only.id = "official-profile-two".into();
    retired_only.source_provider_id = Some("openai-two".into());
    retired_only.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    retired_only.normalize();

    let mut relay = ProviderProfile::new("Relay");
    relay.id = "relay".into();
    relay.base_url = "https://relay.example/v1".into();
    relay.api_key = "relay-key".into();
    relay.normalize();

    let mut config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official, retired_only, relay],
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    };
    config.selected_models_by_provider.insert(
        "openai".into(),
        vec![
            "gpt-5.6-sol".into(),
            "gpt-5.4".into(),
            "GPT-5.4-Mini".into(),
        ],
    );
    config
        .selected_models_by_provider
        .insert("openai-two".into(), vec!["gpt-5.4-mini".into()]);
    config
        .selected_models_by_provider
        .insert("relay".into(), vec!["gpt-5.4".into()]);

    let config = config.normalize();

    assert_eq!(
        config.selected_models_by_provider["openai"],
        ["gpt-5.6-sol"]
    );
    assert!(
        !config
            .selected_models_by_provider
            .contains_key("openai-two")
    );
    assert_eq!(config.selected_models_by_provider["relay"], ["gpt-5.4"]);
}

#[test]
fn mixed_websocket_routes_enable_only_websocket_models() {
    let mut websocket_route = ProviderProfile::new("WS Route");
    websocket_route.id = "route-ws".into();
    websocket_route.base_url = "https://ws.example/v1".into();
    websocket_route.api_key = "ws-key".into();
    websocket_route.supports_websockets = true;
    websocket_route.normalize();

    let mut http_route = ProviderProfile::new("HTTP Route");
    http_route.id = "route-http".into();
    http_route.base_url = "https://http.example/v1".into();
    http_route.api_key = "http-key".into();
    http_route.normalize();

    let mut config = CodeyConfig {
        active_profile_id: websocket_route.id.clone(),
        profiles: vec![websocket_route, http_route],
        ..CodeyConfig::default()
    }
    .normalize();
    config.selected_models_by_provider.insert(
        "route-ws".into(),
        vec!["shared-model".into(), "ws-only".into()],
    );
    config
        .selected_models_by_provider
        .insert("route-http".into(), vec!["shared-model".into()]);

    assert!(config.runtime_supports_websockets());
    assert_eq!(
        config.runtime_websocket_model_aliases(),
        vec![
            local_router::model_alias("route-ws", "shared-model"),
            local_router::model_alias("route-ws", "ws-only"),
        ]
    );

    config.profiles[1].supports_websockets = true;
    assert!(config.runtime_supports_websockets());
    assert_eq!(
        config.runtime_websocket_model_aliases(),
        vec![
            local_router::model_alias("route-ws", "shared-model"),
            local_router::model_alias("route-ws", "ws-only"),
            local_router::model_alias("route-http", "shared-model"),
        ]
    );
}

#[test]
fn native_web_search_model_aliases_require_an_explicit_responses_route() {
    let mut search_route = ProviderProfile::new("Search Route");
    search_route.id = "route-search".into();
    search_route.base_url = "https://search.example/v1".into();
    search_route.api_key = "search-key".into();
    search_route.supports_native_web_search = true;
    search_route.normalize();

    let mut regular_route = ProviderProfile::new("Regular Route");
    regular_route.id = "route-regular".into();
    regular_route.base_url = "https://regular.example/v1".into();
    regular_route.api_key = "regular-key".into();
    regular_route.normalize();

    let mut chat_route = ProviderProfile::new("Chat Route");
    chat_route.id = "route-chat".into();
    chat_route.base_url = "https://chat.example/v1".into();
    chat_route.api_key = "chat-key".into();
    chat_route.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    chat_route.supports_native_web_search = true;
    chat_route.normalize();
    assert!(!chat_route.supports_native_web_search);

    let mut config = CodeyConfig {
        active_profile_id: search_route.id.clone(),
        profiles: vec![search_route, regular_route, chat_route],
        ..CodeyConfig::default()
    }
    .normalize();
    for provider_id in ["route-search", "route-regular", "route-chat"] {
        config
            .selected_models_by_provider
            .insert(provider_id.into(), vec!["gpt-5.6-sol".into()]);
    }

    assert_eq!(
        config.runtime_native_web_search_model_aliases(),
        vec![local_router::model_alias("route-search", "gpt-5.6-sol")]
    );
}

#[test]
fn image_detail_original_requires_a_native_responses_route() {
    let mut responses_route = ProviderProfile::new("原生线路");
    responses_route.id = "route-native".into();
    responses_route.base_url = "https://native.example/v1".into();
    responses_route.api_key = "native-key".into();
    responses_route.normalize();

    let mut chat_route = ProviderProfile::new("兼容线路");
    chat_route.id = "route-chat".into();
    chat_route.base_url = "https://chat.example/v1".into();
    chat_route.api_key = "chat-key".into();
    chat_route.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    chat_route.normalize();

    let mut anthropic_route = ProviderProfile::new("anthropic 线路");
    anthropic_route.id = "route-anthropic".into();
    anthropic_route.base_url = "https://anthropic.example/v1".into();
    anthropic_route.api_key = "anthropic-key".into();
    anthropic_route.upstream_protocol = UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    anthropic_route.normalize();

    let mut disabled_route = ProviderProfile::new("停用线路");
    disabled_route.id = "route-off".into();
    disabled_route.base_url = "https://off.example/v1".into();
    disabled_route.api_key = "off-key".into();
    disabled_route.enabled = false;
    disabled_route.normalize();

    let mut config = CodeyConfig {
        active_profile_id: responses_route.id.clone(),
        profiles: vec![responses_route, chat_route, anthropic_route, disabled_route],
        ..CodeyConfig::default()
    }
    .normalize();
    for provider_id in ["route-native", "route-chat", "route-anthropic", "route-off"] {
        config
            .selected_models_by_provider
            .insert(provider_id.into(), vec!["shared-model".into()]);
    }

    assert_eq!(
        config.runtime_image_detail_original_model_aliases(),
        vec![local_router::model_alias("route-native", "shared-model")]
    );
}

#[test]
fn remote_compaction_protocol_migrates_and_normalizes_official_routes() {
    let mut legacy = serde_json::to_value(ProviderProfile::new("Relay")).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("remoteCompactionProtocol");
    let mut profile: ProviderProfile = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        profile.remote_compaction_protocol,
        RemoteCompactionProtocol::Responses
    );
    profile.remote_compaction_protocol = RemoteCompactionProtocol::CompactEndpoint;
    let serialized = serde_json::to_value(&profile).unwrap();
    assert_eq!(serialized["remoteCompactionProtocol"], "compactEndpoint");
    let decoded: ProviderProfile = serde_json::from_value(serialized).unwrap();
    assert_eq!(
        decoded.remote_compaction_protocol,
        RemoteCompactionProtocol::CompactEndpoint
    );
    profile.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    profile.normalize();
    assert_eq!(
        profile.remote_compaction_protocol,
        RemoteCompactionProtocol::Responses
    );
}

#[test]
fn remote_compaction_blockers_match_runtime_eligibility() {
    let mut capable = ProviderProfile::new("Responses");
    capable.id = "capable".into();
    capable.base_url = "https://relay.example/v1".into();
    capable.supports_remote_compaction = true;
    let mut disabled = capable.clone();
    disabled.id = "disabled".into();
    disabled.enabled = false;
    disabled.supports_remote_compaction = false;
    let mut unavailable = ProviderProfile::new("Unavailable official");
    unavailable.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    unavailable.normalize();
    let mut config = CodeyConfig {
        profiles: vec![
            capable.clone(),
            disabled,
            unavailable,
            ProviderProfile::new("Empty URL"),
        ],
        ..CodeyConfig::default()
    };
    assert!(config.runtime_supports_remote_compaction());
    assert!(config.remote_compaction_blockers().is_empty());

    let mut switched_off = capable.clone();
    switched_off.id = "off".into();
    switched_off.name.clear();
    switched_off.short_name = "备".into();
    switched_off.supports_remote_compaction = false;
    let mut adapted = capable.clone();
    adapted.id = "chat".into();
    adapted.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    let mut plugin = capable;
    plugin.id = "plugin".into();
    plugin.plugin_route_spec = Some(crate::codey_plugins::PluginRouteSpec {
        name: "Plugin".into(),
        base_url: "https://unused.invalid".into(),
        upstream_protocol: UPSTREAM_PROTOCOL_OPENAI_RESPONSES.into(),
        models: vec![],
        model_reasoning_efforts: BTreeMap::new(),
        headers: BTreeMap::new(),
        short_name: String::new(),
        transport: Some(codey_plugin_sdk::transport::TransportOptions {
            account_email: "user@example.com".into(),
            models: BTreeMap::new(),
            image_generation: false,
            image_edit: false,
        }),
    });
    config.profiles.extend([switched_off, adapted, plugin]);
    assert!(config.runtime_supports_remote_compaction());
    assert_eq!(config.remote_compaction_mode(), "mixed");
    let blockers = config.remote_compaction_blockers();
    assert_eq!(blockers.len(), 3);
    assert_eq!(blockers[0].route_id, "off");
    assert_eq!(blockers[0].route_name, "备");
    assert_eq!(blockers[0].reason, "未开启远程压缩");
    assert_eq!(blockers[1].reason, "上游协议不支持原生压缩");
    assert_eq!(blockers[2].reason, "插件传输不支持原生压缩");
    config.profiles.clear();
    assert!(!config.runtime_supports_remote_compaction());
    assert!(config.remote_compaction_blockers().is_empty());
}

#[test]
fn remote_compaction_uses_one_provider_for_mixed_routes() {
    let mut capable = ProviderProfile::new("Responses Route");
    capable.id = "route-capable".into();
    capable.base_url = "https://responses.example/v1".into();
    capable.api_key = "responses-key".into();
    capable.supports_remote_compaction = true;
    capable.normalize();

    let mut config = CodeyConfig {
        active_profile_id: capable.id.clone(),
        profiles: vec![capable],
        ..CodeyConfig::default()
    }
    .normalize();
    assert!(config.runtime_supports_remote_compaction());

    let mut unsupported = ProviderProfile::new("Chat Route");
    unsupported.id = "route-chat".into();
    unsupported.base_url = "https://chat.example/v1".into();
    unsupported.api_key = "chat-key".into();
    unsupported.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    unsupported.supports_remote_compaction = true;
    unsupported.normalize();
    config.profiles.push(unsupported);
    assert!(config.runtime_supports_remote_compaction());
    assert_eq!(config.remote_compaction_mode(), "mixed");
    assert_eq!(
        config.runtime_gateway_provider_id_for_profile(&config.profiles[0]),
        local_router::ROUTER_PROVIDER_ID
    );
    assert_eq!(
        config.runtime_gateway_provider_id_for_profile(&config.profiles[1]),
        local_router::ROUTER_PROVIDER_ID
    );
    config.selected_models_by_provider = BTreeMap::from([
        ("route-capable".into(), vec!["shared-model".into()]),
        ("route-chat".into(), vec!["shared-model".into()]),
    ]);
    assert_eq!(
        config.runtime_remote_compaction_model_aliases(),
        ["route-capable/shared-model"]
    );
}

#[test]
fn official_remote_compaction_requires_the_account_this_launch() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    let mut config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official],
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    }
    .normalize();

    assert!(config.runtime_supports_remote_compaction());

    let mut chat = ProviderProfile::new("Chat Relay");
    chat.id = "chat-route".into();
    chat.base_url = "https://chat.example/v1".into();
    chat.api_key = "chat-key".into();
    chat.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    chat.normalize();
    config.profiles.push(chat);
    assert!(
        config.runtime_supports_remote_compaction(),
        "the official account remains eligible when another route uses local compaction"
    );

    config.official_account_available_this_launch = false;
    assert!(!config.runtime_supports_remote_compaction());
}

#[test]
fn official_websocket_models_enable_automatically_only_when_login_is_available() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();

    let mut config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official],
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    }
    .normalize();
    config
        .selected_models_by_provider
        .insert("openai".into(), vec!["gpt-5.6-sol".into()]);

    assert!(config.runtime_supports_websockets());
    assert_eq!(
        config.runtime_websocket_model_aliases(),
        vec!["gpt-5.6-sol"]
    );

    config.official_account_available_this_launch = false;
    assert!(!config.runtime_supports_websockets());
    assert!(config.runtime_websocket_model_aliases().is_empty());
}

#[test]
fn official_native_web_search_models_require_login_this_launch() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();

    let mut config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official],
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    }
    .normalize();
    config
        .selected_models_by_provider
        .insert("openai".into(), vec!["gpt-5.6-sol".into()]);

    assert_eq!(
        config.runtime_native_web_search_model_aliases(),
        vec!["gpt-5.6-sol"]
    );
    config.official_account_available_this_launch = false;
    assert!(config.runtime_native_web_search_model_aliases().is_empty());
}

#[test]
fn effective_proxy_disables_runtime_websocket_route_advertising() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    let config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official],
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    }
    .normalize();

    assert!(!config.route_supports_websockets_this_launch_with_proxy(&config.profiles[0], true));
}

#[test]
fn normalizes_missing_active_profile() {
    let config = CodeyConfig {
        active_profile_id: "missing".to_string(),
        ..CodeyConfig::default()
    };
    let normalized = config.normalize();
    assert_eq!(normalized.active_profile_id, normalized.profiles[0].id);
}

#[test]
fn disabled_routes_default_compatibility_and_active_fallback() {
    let legacy: ProviderProfile = serde_json::from_value(serde_json::json!({
        "id": "legacy", "name": "Legacy", "baseUrl": "https://example.com"
    }))
    .unwrap();
    assert!(legacy.enabled);
    let mut disabled = legacy.clone();
    disabled.enabled = false;
    let mut enabled = ProviderProfile::new("Enabled");
    enabled.id = "enabled".into();
    let mut config = CodeyConfig {
        active_profile_id: disabled.id.clone(),
        profiles: vec![disabled, enabled],
        ..CodeyConfig::default()
    };
    config
        .selected_models_by_provider
        .insert("legacy".into(), vec!["old-model".into()]);
    config = config.normalize();
    assert_eq!(config.active_profile_id, "enabled");
    assert!(config.runtime_catalog_models().1.is_empty());
    assert!(config.runtime_model_targets().is_empty());
    config.profiles[1].enabled = false;
    let config = config.normalize();
    assert!(config.runtime_model_targets().is_empty());
    assert!(!config.has_third_party_route());
}

#[test]
fn empty_default_route_is_not_a_third_party_route() {
    let config = CodeyConfig::default();
    assert!(config.profiles[0].is_unconfigured_default());
    assert!(config.needs_initial_route_import());
    assert!(!config.has_third_party_route());
    assert!(validate_provider_profiles(&config.profiles).is_ok());
}

#[test]
fn non_empty_legacy_configs_are_marked_as_imported() {
    let mut route = ProviderProfile::new("Relay");
    route.id = "relay".into();
    route.base_url = "https://relay.example/v1".into();
    route.api_key = "sk-relay".into();
    route.normalize();
    let config = CodeyConfig {
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        initial_route_import_completed: false,
        ..CodeyConfig::default()
    }
    .normalize();

    assert!(config.initial_route_import_completed);
}

#[test]
fn launch_official_profile_is_first_without_stealing_active_third_party_route() {
    let mut relay = ProviderProfile::new("Relay");
    relay.id = "relay".into();
    relay.base_url = "https://relay.example/v1".into();
    relay.api_key = "sk-relay".into();
    relay.normalize();
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = "openai-source".into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    let mut config = CodeyConfig {
        active_profile_id: relay.id.clone(),
        profiles: vec![relay],
        initial_route_import_completed: true,
        ..CodeyConfig::default()
    };
    config.default_model = "openai/gpt-5.6-sol".into();

    config.apply_launch_official_profiles(vec![official]);
    config = config.normalize();

    assert_eq!(config.profiles[0].id, DERIVED_OFFICIAL_PROFILE_ID);
    assert_eq!(config.active_profile_id, "relay");
    assert_eq!(config.profiles[0].provider_id(), "openai");
    assert_eq!(config.default_model, "openai/gpt-5.6-sol");
    assert_eq!(
        config.selected_models_by_provider["openai"],
        model_catalog::default_official_model_slugs(),
    );

    config
        .selected_models_by_provider
        .insert("openai".into(), vec!["gpt-5.6-sol".into()]);
    config = config.normalize();
    assert_eq!(
        config.selected_models_by_provider["openai"],
        ["gpt-5.6-sol"],
    );
}

#[test]
fn launch_official_profile_migrates_legacy_official_provider_state() {
    let mut legacy = ProviderProfile::new("OpenAI 官方直登");
    legacy.id = DERIVED_OFFICIAL_PROFILE_ID.into();
    legacy.source_provider_id = Some("local-official".into());
    legacy.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    legacy.normalize();

    let mut launched = ProviderProfile::new("OpenAI 官方直登");
    launched.id = "launch-openai".into();
    launched.source_provider_id = Some("openai".into());
    launched.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    launched.normalize();

    let mut config = CodeyConfig {
        active_profile_id: legacy.id.clone(),
        profiles: vec![legacy],
        initial_route_import_completed: true,
        default_model: "local-official/gpt-5.6-terra".into(),
        subagent_model: "local-official/gpt-5.6-luna".into(),
        subagent_reasoning_effort: "high".into(),
        subagent_roles: uniform_subagent_roles("local-official/gpt-5.6-luna", "high"),
        ..CodeyConfig::default()
    };
    config
        .selected_models_by_provider
        .insert("local-official".into(), vec!["gpt-5.6-terra".into()]);
    config
        .manual_third_party_models_by_provider
        .insert("local-official".into(), vec!["manual-model".into()]);
    config
        .declared_official_models_by_provider
        .insert("local-official".into(), vec!["gpt-5.6-luna".into()]);
    config
        .upstream_models_by_provider
        .insert("local-official".into(), vec!["gpt-5.6-terra".into()]);
    config.default_model = "local-official/gpt-5.6-terra".into();

    config.apply_launch_official_profiles(vec![launched]);

    assert_eq!(config.profiles.len(), 1);
    assert_eq!(config.profiles[0].id, DERIVED_OFFICIAL_PROFILE_ID);
    assert_eq!(config.profiles[0].provider_id(), "openai");
    assert_eq!(config.active_profile_id, DERIVED_OFFICIAL_PROFILE_ID);
    assert_eq!(
        config.selected_models_by_provider["openai"],
        ["gpt-5.6-terra"]
    );
    assert_eq!(
        config.manual_third_party_models_by_provider["openai"],
        ["manual-model"]
    );
    assert_eq!(
        config.declared_official_models_by_provider["openai"],
        ["gpt-5.6-luna"]
    );
    assert_eq!(
        config.upstream_models_by_provider["openai"],
        ["gpt-5.6-terra"]
    );
    assert_eq!(config.default_model, "openai/gpt-5.6-terra");
    assert_eq!(config.subagent_model, "openai/gpt-5.6-luna");
    assert!(
        config
            .subagent_roles
            .values()
            .all(|selection| selection.model == "openai/gpt-5.6-luna")
    );
    assert!(
        !config
            .selected_models_by_provider
            .contains_key("local-official")
    );
    assert!(
        !config
            .manual_third_party_models_by_provider
            .contains_key("local-official")
    );
    assert!(
        !config
            .declared_official_models_by_provider
            .contains_key("local-official")
    );
    assert!(
        !config
            .upstream_models_by_provider
            .contains_key("local-official")
    );
}

#[test]
fn launch_official_profiles_give_every_stored_account_its_own_route() {
    let mut first = ProviderProfile::new("主力账号");
    first.source_provider_id = Some("openai".into());
    first.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    first.official_account_id = Some("acct-one".into());
    first.normalize();
    let mut second = ProviderProfile::new("备用账号");
    second.source_provider_id = Some("openai".into());
    second.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    second.official_account_id = Some("acct-two".into());
    second.normalize();

    let mut config = CodeyConfig {
        initial_route_import_completed: true,
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    };

    config.apply_launch_official_profiles(vec![first, second]);
    let config = config.normalize();

    assert_eq!(config.profiles.len(), 2);
    assert_eq!(config.profiles[0].id, official_profile_id("acct-one"));
    assert_eq!(config.profiles[1].id, official_profile_id("acct-two"));
    assert_eq!(
        config.profiles[0].official_account_id.as_deref(),
        Some("acct-one")
    );
    assert_eq!(
        config.profiles[1].official_account_id.as_deref(),
        Some("acct-two")
    );
    assert_eq!(config.profiles[0].display_short_name(), "主力");
    assert_eq!(config.profiles[1].display_short_name(), "备用");
    assert!(config.qualifies_official_model_ids());

    // 同一段对话里两条官方线路的模型必须能区分，模型名带上线路前缀。
    let (_, selected) = config.runtime_catalog_models();
    let first_alias = local_router::model_alias(config.profiles[0].provider_id(), "gpt-5.6-sol");
    let second_alias = local_router::model_alias(config.profiles[1].provider_id(), "gpt-5.6-sol");
    assert!(selected.contains(&first_alias), "{selected:?}");
    assert!(selected.contains(&second_alias), "{selected:?}");
    assert!(
        !selected.contains(&"gpt-5.6-sol".to_string()),
        "{selected:?}"
    );

    let targets = config.runtime_model_targets();
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.official && target.upstream_model == "gpt-5.6-sol")
            .count(),
        2
    );
}

#[test]
fn a_single_stored_account_keeps_native_official_model_ids() {
    let mut only = ProviderProfile::new("主力账号");
    only.source_provider_id = Some("openai".into());
    only.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    only.official_account_id = Some("acct-one".into());
    only.normalize();

    let mut config = CodeyConfig {
        initial_route_import_completed: true,
        ..CodeyConfig::default()
    };
    // 默认登录缺失时，存储账号仍然可以经本地路由直接使用。
    config.official_account_available_this_launch = false;
    config.apply_launch_official_profiles(vec![only]);
    let config = config.normalize();

    assert!(!config.qualifies_official_model_ids());
    let (_, selected) = config.runtime_catalog_models();
    assert!(
        selected.contains(&"gpt-5.6-sol".to_string()),
        "{selected:?}"
    );
}

#[test]
fn api_key_launch_removes_derived_official_route_and_falls_back_to_saved_route() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    let mut relay = ProviderProfile::new("Relay");
    relay.id = "relay".into();
    relay.base_url = "https://relay.example/v1".into();
    relay.api_key = "sk-relay".into();
    relay.normalize();
    let mut config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official, relay],
        initial_route_import_completed: true,
        ..CodeyConfig::default()
    };
    config.default_model = "gpt-5.6-sol".into();

    config.apply_launch_official_profiles(Vec::new());
    config = config.normalize();

    assert_eq!(config.profiles.len(), 1);
    assert_eq!(config.profiles[0].id, "relay");
    assert_eq!(config.active_profile_id, "relay");
    assert_eq!(config.default_model, "gpt-5.6-sol");
}

#[test]
fn preserves_an_empty_upstream_snapshot_as_a_successful_sync() {
    let mut config = CodeyConfig::default();
    let provider_id = config.current_provider_id().unwrap().to_string();
    config
        .upstream_models_by_provider
        .insert(provider_id, Vec::new());

    let normalized = config.normalize();

    assert_eq!(normalized.upstream_models_snapshot(), Some([].as_slice()));
}

#[test]
fn model_lists_trim_and_dedupe_case_insensitively() {
    let mut config = CodeyConfig::default();
    let provider_id = config.current_provider_id().unwrap().to_string();
    config.selected_models_by_provider.insert(
        provider_id.clone(),
        vec![
            " Provider-A ".to_string(),
            "provider-a".to_string(),
            "Provider-B".to_string(),
        ],
    );
    config.upstream_models_by_provider.insert(
        provider_id.clone(),
        vec!["UPSTREAM-A".to_string(), "upstream-a".to_string()],
    );
    config.declared_official_models_by_provider.insert(
        provider_id.clone(),
        vec![" GPT-5.6-SOL ".to_string(), "gpt-5.6-sol".to_string()],
    );

    let normalized = config.normalize();

    assert_eq!(
        normalized.selected_models_by_provider[&provider_id],
        ["Provider-A", "Provider-B"]
    );
    assert_eq!(
        normalized.upstream_models_by_provider[&provider_id],
        ["UPSTREAM-A", "gpt-5.6-sol"]
    );
    assert_eq!(
        normalized.declared_official_models_by_provider[&provider_id],
        ["GPT-5.6-SOL"]
    );
}

#[test]
fn diagnostic_guards_can_be_disabled_explicitly() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"disableTraceLogWrites":false,"protectCrashpadPending":false}"#,
    )
    .unwrap()
    .normalize();
    let serialized = serde_json::to_value(&config).unwrap();

    assert!(!config.disable_trace_log_writes);
    assert!(!config.protect_crashpad_pending);
    assert_eq!(
        serialized.get("disableTraceLogWrites"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(
        serialized.get("protectCrashpadPending"),
        Some(&serde_json::json!(false))
    );
}

#[test]
fn legacy_webhook_is_migrated_to_a_feishu_channel_without_the_old_secret() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"webhook":{"enabled":true,"url":"https://open.feishu.cn/example","secret":"legacy-sign-key"}}"#,
    )
    .unwrap()
    .normalize();
    let serialized = serde_json::to_value(&config).unwrap();

    assert!(!config.webhook.enabled);
    assert!(config.webhook.url.is_empty());
    assert_eq!(config.webhook.channels.len(), 1);
    let channel = &config.webhook.channels[0];
    assert_eq!(channel.id, "legacy-feishu");
    assert_eq!(
        channel.kind,
        crate::notifications::NotificationChannelKind::Feishu
    );
    assert!(channel.enabled);
    assert_eq!(channel.url, "https://open.feishu.cn/example");
    assert!(serialized["webhook"].get("enabled").is_none());
    assert!(serialized["webhook"].get("url").is_none());
    assert!(serialized["webhook"].get("secret").is_none());
}

#[test]
fn trace_log_guard_defaults_to_enabled_for_existing_configs() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();

    assert!(config.disable_trace_log_writes);
    assert!(config.protect_crashpad_pending);
}

#[test]
fn local_router_defaults_to_enabled_for_existing_configs() {
    let legacy = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();
    let disabled = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"localRouterEnabled":false}"#,
    )
    .unwrap()
    .normalize();

    assert!(legacy.local_router_enabled);
    assert!(!disabled.local_router_enabled);
    assert_eq!(
        serde_json::to_value(disabled).unwrap()["localRouterEnabled"],
        serde_json::json!(false)
    );
}

#[test]
fn codey_update_policy_migrates_legacy_and_preserves_channels() {
    assert_eq!(
        CodeyConfig::default().codey_update_policy,
        CodeyUpdatePolicy::Stable
    );
    let legacy = serde_json::from_str::<CodeyConfig>(r#"{}"#)
        .unwrap()
        .normalize();
    assert_eq!(legacy.codey_update_policy, CodeyUpdatePolicy::Stable);

    for enabled in [false, true] {
        let config = serde_json::from_value::<CodeyConfig>(serde_json::json!({
            "autoCheckCodeyUpdates": enabled,
        }))
        .unwrap()
        .normalize();
        let expected = if enabled {
            CodeyUpdatePolicy::Stable
        } else {
            CodeyUpdatePolicy::Off
        };
        assert_eq!(config.codey_update_policy, expected);
        assert_eq!(
            serde_json::to_value(config).unwrap()["codeyUpdatePolicy"],
            expected.as_str()
        );
    }
    for policy in ["off", "stable", "experimental"] {
        let config: CodeyConfig =
            serde_json::from_value(serde_json::json!({"codeyUpdatePolicy": policy})).unwrap();
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(serialized["codeyUpdatePolicy"], policy);
        assert!(serialized.get("autoCheckCodeyUpdates").is_none());
    }
    assert!(
        serde_json::from_value::<CodeyConfig>(serde_json::json!({"codeyUpdatePolicy": "unknown"}))
            .is_err()
    );
}

#[test]
fn route_request_log_is_opt_in_and_normalizes_resource_bounds() {
    let legacy = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();
    assert!(!legacy.route_request_log.enabled);

    let configured = serde_json::from_str::<CodeyConfig>(
        r#"{
            "activeProfileId":"",
            "profiles":[],
            "routeRequestLog":{
                "enabled":true,
                "backend":"sqlite",
                "queueCapacity":1,
                "batchSize":999999,
                "flushIntervalMs":1,
                "shutdownFlushTimeoutMs":999999,
                "sampleRatePerMillion":9999999,
                "maxFileBytes":1,
                "retainedFiles":0,
                "retentionDays":0
            }
        }"#,
    )
    .unwrap()
    .normalize();
    assert!(configured.route_request_log.enabled);
    assert_eq!(
        configured.route_request_log.backend,
        RouteRequestLogBackend::Sqlite
    );
    assert_eq!(configured.route_request_log.queue_capacity, 128);
    assert_eq!(configured.route_request_log.batch_size, 128);
    assert_eq!(configured.route_request_log.flush_interval_ms, 50);
    assert_eq!(
        configured.route_request_log.shutdown_flush_timeout_ms,
        10_000
    );
    assert_eq!(
        configured.route_request_log.sample_rate_per_million,
        1_000_000
    );
    assert_eq!(configured.route_request_log.max_file_bytes, 1024 * 1024);
    assert_eq!(configured.route_request_log.retained_files, 1);
    assert_eq!(configured.route_request_log.retention_days, 1);
}

#[test]
fn user_update_manifest_url_is_ignored_and_not_persisted() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"updateManifestUrl":"https://example.com/latest.json"}"#,
    )
    .unwrap()
    .normalize();
    let serialized = serde_json::to_value(&config).unwrap();

    assert_eq!(config.update_manifest_url, default_update_manifest_url());
    assert!(serialized.get("updateManifestUrl").is_none());
}

#[test]
fn update_manifest_url_defaults_to_the_public_source_for_local_builds() {
    let expected = format!("{DEFAULT_UPDATE_BASE_URL}/latest.json");

    assert_eq!(update_manifest_url_from_base(None), expected);
    assert_eq!(update_manifest_url_from_base(Some("  ")), expected);
    assert_eq!(
        update_manifest_url_from_base(Some("https://updates.example.com/codey/")),
        "https://updates.example.com/codey/latest.json"
    );
}

#[test]
fn pet_slim_mode_defaults_to_enabled_for_existing_configs() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();

    assert!(config.slim_codex_pet);
}

#[test]
fn pet_slim_mode_can_be_disabled_explicitly() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"slimCodexPet":false}"#,
    )
    .unwrap()
    .normalize();

    assert!(!config.slim_codex_pet);
}

#[test]
fn gpu_launch_mode_defaults_to_off() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();
    let serialized = serde_json::to_value(&config).unwrap();

    assert_eq!(config.gpu_launch_mode, GpuLaunchMode::Off);
    assert_eq!(serialized["gpuLaunchMode"], "off");
}

#[test]
fn gpu_launch_modes_round_trip_as_mutually_exclusive_values() {
    for (wire_value, expected) in [
        ("off", GpuLaunchMode::Off),
        ("disableGpu", GpuLaunchMode::DisableGpu),
        (
            "disableGpuRasterization",
            GpuLaunchMode::DisableGpuRasterization,
        ),
    ] {
        let config = serde_json::from_value::<CodeyConfig>(serde_json::json!({
            "activeProfileId": "",
            "profiles": [],
            "gpuLaunchMode": wire_value,
        }))
        .unwrap()
        .normalize();

        assert_eq!(config.gpu_launch_mode, expected);
        assert_eq!(
            serde_json::to_value(&config).unwrap()["gpuLaunchMode"],
            wire_value
        );
    }
}

#[test]
fn fast_context_tools_default_to_disabled_for_existing_configs() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();

    assert!(!config.fast_context_tools);
}

#[test]
fn retired_fast_startup_setting_is_ignored_and_removed_on_serialize() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"fastCodexStartup":true}"#,
    )
    .unwrap()
    .normalize();

    let serialized = serde_json::to_value(config).unwrap();
    assert!(serialized.get("fastCodexStartup").is_none());
}

#[test]
fn subagent_optimization_defaults_to_disabled_for_existing_configs() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();

    assert!(!config.subagent_optimization);
    assert_eq!(config.subagent_model, DEFAULT_SUBAGENT_MODEL);
    assert_eq!(
        config.subagent_reasoning_effort,
        DEFAULT_SUBAGENT_REASONING_EFFORT
    );
    assert_eq!(config.subagent_roles.len(), SUBAGENT_ROLE_IDS.len());
    assert!(
        config
            .subagent_roles
            .values()
            .all(|selection| selection.enabled && selection.model == DEFAULT_SUBAGENT_MODEL)
    );
}

#[test]
fn fresh_subagent_defaults_keep_the_original_role_preset() {
    let config = CodeyConfig::default();

    assert!(
        config
            .subagent_roles
            .values()
            .all(|selection| selection.model == DEFAULT_SUBAGENT_MODEL)
    );
    assert_eq!(
        config.subagent_roles[SUBAGENT_ROLE_WORKER].reasoning_effort,
        "medium"
    );
    assert_eq!(
        config.subagent_roles[SUBAGENT_ROLE_VISUAL_WORKER].reasoning_effort,
        "high"
    );
}

#[test]
fn legacy_subagent_roles_default_to_enabled_and_explicit_disables_survive_normalization() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{
            "activeProfileId":"",
            "profiles":[],
            "subagentRoles":{
                "codey_worker":{"enabled":false,"model":"worker-model","reasoningEffort":"high"},
                "default":{"model":"fallback-model","reasoningEffort":"medium"}
            }
        }"#,
    )
    .unwrap()
    .normalize();

    assert!(!config.subagent_roles[SUBAGENT_ROLE_WORKER].enabled);
    assert!(config.subagent_roles[SUBAGENT_ROLE_QUICK_SCAN].enabled);
    assert!(config.subagent_roles[SUBAGENT_ROLE_DEFAULT].enabled);
}

#[test]
fn subagent_models_use_unique_router_aliases_without_changing_explicit_routes() {
    for (router_enabled, duplicate, requested, expected) in [
        (true, false, " worker-model ", "route-ws/worker-model"),
        (true, false, "WORKER-MODEL", "route-ws/worker-model"),
        (false, false, "worker-model", "worker-model"),
        (true, true, "worker-model", "worker-model"),
        (
            true,
            true,
            "route-http/worker-model",
            "route-http/worker-model",
        ),
        (true, false, "unknown-model", "unknown-model"),
        (true, false, "vendor/model", "route-ws/vendor/model"),
    ] {
        let mut websocket_route = ProviderProfile::new("WS Route");
        websocket_route.id = "route-ws".into();
        websocket_route.supports_websockets = true;
        let mut http_route = ProviderProfile::new("HTTP Route");
        http_route.id = "route-http".into();
        let config = CodeyConfig {
            local_router_enabled: router_enabled,
            active_profile_id: http_route.id.clone(),
            profiles: vec![websocket_route, http_route],
            selected_models_by_provider: BTreeMap::from([
                (
                    "route-ws".into(),
                    vec!["worker-model".into(), "vendor/model".into()],
                ),
                (
                    "route-http".into(),
                    vec![
                        if duplicate {
                            "worker-model"
                        } else {
                            "main-model"
                        }
                        .into(),
                    ],
                ),
            ]),
            default_model: "route-http/main-model".into(),
            subagent_model: requested.into(),
            subagent_roles: uniform_subagent_roles(requested, "high"),
            ..CodeyConfig::default()
        }
        .normalize();

        assert_eq!(config.subagent_model, expected, "requested: {requested}");
        assert!(config.subagent_roles.values().all(|selection| {
            selection.model == expected && selection.reasoning_effort == "high"
        }));
        assert_eq!(config.clone().normalize(), config);
    }
}

#[test]
fn subagent_defaults_preserve_models_and_invalid_effort_falls_back() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"subagentModel":"  provider-coder  ","subagentReasoningEffort":"unsupported"}"#,
    )
    .unwrap()
    .normalize();

    assert_eq!(config.subagent_model, "provider-coder");
    assert_eq!(
        config.subagent_reasoning_effort,
        DEFAULT_SUBAGENT_REASONING_EFFORT
    );
    assert!(config.subagent_roles.values().all(|selection| {
        selection.model == "provider-coder"
            && selection.reasoning_effort == DEFAULT_SUBAGENT_REASONING_EFFORT
    }));

    let empty = serde_json::from_str::<CodeyConfig>(
        r#"{"activeProfileId":"","profiles":[],"subagentModel":"   ","subagentReasoningEffort":"high"}"#,
    )
    .unwrap()
    .normalize();

    assert_eq!(empty.subagent_model, DEFAULT_SUBAGENT_MODEL);
    assert_eq!(empty.subagent_reasoning_effort, "high");
}

#[test]
fn subagent_role_map_normalizes_independently_and_syncs_the_legacy_fallback() {
    let config = serde_json::from_str::<CodeyConfig>(
        r#"{
            "activeProfileId":"",
            "profiles":[],
            "subagentModel":"legacy-model",
            "subagentReasoningEffort":"low",
            "subagentRoles":{
                "codey_quick_scan":{"model":" quick-model ","reasoningEffort":"MEDIUM"},
                "default":{"model":" fallback-model ","reasoningEffort":"high"},
                "unknown":{"model":"ignored","reasoningEffort":"low"}
            }
        }"#,
    )
    .unwrap()
    .normalize();

    assert_eq!(config.subagent_roles.len(), SUBAGENT_ROLE_IDS.len());
    assert!(!config.subagent_roles.contains_key("unknown"));
    assert_eq!(
        config.subagent_roles[SUBAGENT_ROLE_QUICK_SCAN],
        SubagentRoleConfig::new("quick-model", "medium")
    );
    assert_eq!(config.subagent_model, "fallback-model");
    assert_eq!(config.subagent_reasoning_effort, "high");
    assert_eq!(
        config.subagent_roles[SUBAGENT_ROLE_WORKER],
        SubagentRoleConfig::new("fallback-model", "high")
    );
}

#[test]
fn subagent_config_is_global_and_obsolete_provider_entries_are_ignored() {
    let mut provider_a = ProviderProfile::new("A");
    provider_a.id = "provider-a".into();
    let mut provider_b = ProviderProfile::new("B");
    provider_b.id = "provider-b".into();
    let mut config = CodeyConfig {
        active_profile_id: provider_a.id.clone(),
        profiles: vec![provider_a, provider_b],
        selected_models_by_provider: BTreeMap::from([
            ("provider-a".into(), vec!["model-a".into()]),
            ("provider-b".into(), vec!["model-b".into()]),
        ]),
        subagent_model: "provider-a/model-a".into(),
        subagent_reasoning_effort: "high".into(),
        subagent_roles: uniform_subagent_roles("provider-a/model-a", "high"),
        ..CodeyConfig::default()
    }
    .normalize();

    assert_eq!(config.subagent_model, "provider-a/model-a");

    config.active_profile_id = "provider-b".into();
    config = config.normalize();
    assert_eq!(config.subagent_model, "provider-a/model-a");
    assert!(
        config
            .subagent_roles
            .values()
            .all(|selection| selection.model == "provider-a/model-a")
    );

    let serialized = serde_json::to_value(&config).unwrap();
    assert!(serialized.get("subagentConfigByProvider").is_none());

    let obsolete = serde_json::from_value::<CodeyConfig>(serde_json::json!({
        "activeProfileId": "",
        "profiles": [],
        "subagentModel": "global-model",
        "subagentReasoningEffort": "high",
        "subagentConfigByProvider": {
            "provider-b": {
                "model": "provider-b/model-b",
                "reasoningEffort": "low",
                "roles": {}
            }
        }
    }))
    .unwrap()
    .normalize();
    assert_eq!(obsolete.subagent_model, "global-model");
    assert!(serde_json::to_value(obsolete).unwrap()["subagentConfigByProvider"].is_null());
}

#[test]
fn full_access_warning_shield_defaults_to_disabled_for_existing_configs() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();

    assert!(!config.hide_full_access_warning);
}

#[test]
fn misc_model_defaults_to_empty_and_normalizes_to_a_unique_route_alias() {
    let empty = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();
    assert!(empty.misc_model.is_empty());
    assert!(empty.misc_model_catalog_id().is_none());

    for (router_enabled, duplicate, requested, expected) in [
        (true, false, " worker-model ", "route-a/worker-model"),
        (true, false, "WORKER-MODEL", "route-a/worker-model"),
        (true, false, "route-a/worker-model", "route-a/worker-model"),
        (true, true, "worker-model", "worker-model"),
        (false, false, "worker-model", "worker-model"),
        (true, false, "unknown-model", "unknown-model"),
    ] {
        let mut route_a = ProviderProfile::new("Route A");
        route_a.id = "route-a".into();
        let mut route_b = ProviderProfile::new("Route B");
        route_b.id = "route-b".into();
        let config = CodeyConfig {
            local_router_enabled: router_enabled,
            active_profile_id: route_a.id.clone(),
            profiles: vec![route_a, route_b],
            selected_models_by_provider: BTreeMap::from([
                ("route-a".into(), vec!["worker-model".into()]),
                (
                    "route-b".into(),
                    vec![
                        if duplicate {
                            "worker-model"
                        } else {
                            "other-model"
                        }
                        .into(),
                    ],
                ),
            ]),
            misc_model: requested.into(),
            ..CodeyConfig::default()
        }
        .normalize();

        assert_eq!(config.misc_model, expected, "requested: {requested}");
        assert_eq!(config.clone().normalize(), config);
    }
}

#[test]
fn misc_model_uses_only_the_active_route_in_direct_mode() {
    for (requested, expected) in [
        ("route-a/vendor/worker", Some("vendor/worker")),
        ("VENDOR/WORKER", Some("vendor/worker")),
        ("route-b/vendor/worker", None),
        ("missing-model", None),
    ] {
        let mut route_a = ProviderProfile::new("Route A");
        route_a.id = "route-a".into();
        let mut route_b = ProviderProfile::new("Route B");
        route_b.id = "route-b".into();
        let mut config = CodeyConfig {
            local_router_enabled: false,
            active_profile_id: route_a.id.clone(),
            profiles: vec![route_a, route_b],
            selected_models_by_provider: BTreeMap::from([
                ("route-a".into(), vec!["vendor/worker".into()]),
                ("route-b".into(), vec!["vendor/worker".into()]),
            ]),
            misc_model: requested.into(),
            ..CodeyConfig::default()
        }
        .normalize();
        assert_eq!(
            config.misc_model_catalog_id().as_deref(),
            expected,
            "{requested}"
        );
        if let Some(expected) = expected {
            assert_eq!(config.misc_model, expected);
            config.local_router_enabled = true;
            config = config.normalize();
            // 原始模型在多条线路上同名时不能猜测供应商。
            assert!(config.misc_model_catalog_id().is_none());
        }
        assert_eq!(config.clone().normalize(), config);
    }
}

#[test]
fn misc_model_keeps_a_disabled_route_selection_without_using_its_history() {
    let mut route_a = ProviderProfile::new("Route A");
    route_a.id = "route-a".into();
    let mut route_b = ProviderProfile::new("Route B");
    route_b.id = "route-b".into();
    let mut config = CodeyConfig {
        active_profile_id: route_a.id.clone(),
        profiles: vec![route_a, route_b],
        selected_models_by_provider: BTreeMap::from([
            ("route-a".into(), vec!["worker".into()]),
            (
                "route-b".into(),
                vec!["worker".into(), "route-a/worker".into()],
            ),
        ]),
        misc_model: "route-a/worker".into(),
        ..CodeyConfig::default()
    }
    .normalize();
    config.profiles[0].enabled = false;
    config = config.normalize();
    assert_eq!(config.misc_model, "route-a/worker");
    assert!(config.misc_model_target().is_none());
    assert!(config.misc_model_catalog_id().is_none());
    config.profiles[0].enabled = true;
    config = config.normalize();
    assert_eq!(config.misc_model_target().unwrap().provider_id, "route-a");
}

#[test]
fn misc_model_clears_when_its_route_is_removed() {
    let mut route = ProviderProfile::new("Route A");
    route.id = "route-a".into();
    let mut config = CodeyConfig {
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        selected_models_by_provider: BTreeMap::from([(
            "route-a".into(),
            vec!["worker-model".into()],
        )]),
        misc_model: "route-a/worker-model".into(),
        ..CodeyConfig::default()
    }
    .normalize();
    assert_eq!(config.misc_model, "route-a/worker-model");
    assert_eq!(
        config.misc_model_catalog_id().as_deref(),
        Some("route-a/worker-model")
    );

    config.profiles.clear();
    config.selected_models_by_provider.clear();
    config.reconcile_after_route_removal("route-a");
    assert!(config.misc_model.is_empty());
    assert!(config.misc_model_catalog_id().is_none());
}

#[test]
fn misc_model_is_cleared_instead_of_migrating_to_an_equal_model_on_another_route() {
    let mut route_a = ProviderProfile::new("Route A");
    route_a.id = "route-a".into();
    let mut route_b = ProviderProfile::new("Route B");
    route_b.id = "route-b".into();
    let mut config = CodeyConfig {
        active_profile_id: route_a.id.clone(),
        profiles: vec![route_a, route_b],
        selected_models_by_provider: BTreeMap::from([
            ("route-a".into(), vec!["worker-model".into()]),
            ("route-b".into(), vec!["worker-model".into()]),
        ]),
        misc_model: "route-a/worker-model".into(),
        ..CodeyConfig::default()
    }
    .normalize();
    config.remember_model_aliases();

    config.profiles.retain(|profile| profile.id != "route-a");
    config.selected_models_by_provider.remove("route-a");
    config = config.normalize();
    config.reconcile_after_route_removal("route-a");

    assert!(config.misc_model.is_empty());
}

#[test]
fn header_account_usage_defaults_to_enabled_for_supported_configs() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();

    assert!(config.show_account_usage_in_header);
}

#[test]
fn prompt_optimization_defaults_to_disabled_for_existing_configs() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
        .unwrap()
        .normalize();

    assert!(!config.prompt_optimization.enabled);
    assert!(config.prompt_optimization.api_key.is_empty());
}

#[test]
fn prompt_optimization_round_trips_without_persisting_clear_flag() {
    let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[],"promptOptimization":{"enabled":true,"mode":"manual","baseUrl":" https://api.example.com/v1/ ","apiKey":"sk-secret","model":" gpt-x ","upstreamProtocol":"anthropicMessages","instruction":" 保持简洁 "}}"#)
        .unwrap()
        .normalize();
    let serialized = serde_json::to_value(&config).unwrap();

    assert!(config.prompt_optimization.enabled);
    assert_eq!(
        config.prompt_optimization.base_url,
        "https://api.example.com/v1"
    );
    assert_eq!(config.prompt_optimization.api_key, "sk-secret");
    assert!(config.prompt_optimization.api_key_configured);
    assert_eq!(config.prompt_optimization.model, "gpt-x");
    assert_eq!(
        config.prompt_optimization.mode,
        PROMPT_OPTIMIZATION_MODE_MANUAL
    );
    assert_eq!(
        config.prompt_optimization.upstream_protocol,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
    );
    assert_eq!(config.prompt_optimization.instruction, "保持简洁");
    assert_eq!(
        serialized["promptOptimization"]["upstreamProtocol"],
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
    );
    assert!(
        serialized["promptOptimization"]
            .get("clearApiKey")
            .is_none()
    );
}

#[test]
fn redacted_prompt_optimization_key_is_restored_when_other_settings_are_saved() {
    let previous = CodeyConfig {
        prompt_optimization: PromptOptimizationConfig {
            enabled: true,
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "sk-secret".to_string(),
            api_key_configured: true,
            model: "gpt-x".to_string(),
            ..PromptOptimizationConfig::default()
        },
        ..CodeyConfig::default()
    };
    let mut incoming = previous.clone();
    incoming.prompt_optimization.api_key.clear();
    incoming
        .prompt_optimization
        .merge_redacted_secrets(&previous.prompt_optimization);

    assert_eq!(incoming.prompt_optimization.api_key, "sk-secret");
}

#[test]
fn explicit_prompt_optimization_key_clear_does_not_restore_the_previous_secret() {
    let previous = CodeyConfig {
        prompt_optimization: PromptOptimizationConfig {
            api_key: "sk-secret".to_string(),
            api_key_configured: true,
            ..PromptOptimizationConfig::default()
        },
        ..CodeyConfig::default()
    };
    let mut incoming = previous.clone();
    incoming.prompt_optimization.api_key.clear();
    incoming.prompt_optimization.clear_api_key = true;
    incoming
        .prompt_optimization
        .merge_redacted_secrets(&previous.prompt_optimization);

    assert!(incoming.prompt_optimization.api_key.is_empty());
    assert!(!incoming.prompt_optimization.api_key_configured);
}
