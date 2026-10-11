use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::{fs, path::Path};

use reqwest::Client;
use serde_json::{Value, json};
use uuid::Uuid;

use super::AppState;
use crate::codex_config::codex_home;
use crate::config::PromptOptimizationConfig;
use crate::error_log;
use crate::local_router;
use crate::prompt_optimization;

static OPTIMIZER_CLIENT: OnceLock<Client> = OnceLock::new();
static LOOPBACK_OPTIMIZER_CLIENT: OnceLock<Client> = OnceLock::new();
const CODEX_INSTALLATION_ID_HEADER: &str = "x-codex-installation-id";
const CODEX_INSTALLATION_ID_FILE: &str = "installation_id";

pub(super) fn optimizer_client(uses_codey_route: bool) -> Result<&'static Client, String> {
    let slot = if uses_codey_route {
        &LOOPBACK_OPTIMIZER_CLIENT
    } else {
        &OPTIMIZER_CLIENT
    };
    if let Some(client) = slot.get() {
        return Ok(client);
    }
    let client = if uses_codey_route {
        prompt_optimization::loopback_optimizer_http_client()?
    } else {
        prompt_optimization::optimizer_http_client()?
    };
    // Concurrent callers may build a duplicate client; the first successful
    // one wins and the rest reuse it.
    Ok(slot.get_or_init(|| client))
}

pub(super) async fn resolve_request_config(
    state: &Arc<AppState>,
    optimization: &PromptOptimizationConfig,
) -> Result<prompt_optimization::ResolvedPromptOptimizationConfig, String> {
    if optimization.uses_codey_route() {
        let config = state.config.read().await.clone();
        if !config.local_router_enabled {
            return Err("本地路由已关闭；请启用本地路由并重启 Codex，或改用手动配置".to_string());
        }
        let runtime = state.runtime.lock().await.clone().ok_or_else(|| {
            "Codey 路由尚未运行，请先启动 Codey 后再测试或使用提示词优化".to_string()
        })?;
        let endpoint = runtime.local_router_endpoint().ok_or_else(|| {
            "本地路由已关闭；请启用本地路由并重启 Codex，或改用手动配置".to_string()
        })?;
        return Ok(resolve_codey_request_config(
            &config,
            optimization,
            endpoint,
        ));
    }
    resolve_manual_request_config_at(optimization, codex_home())
}

fn resolve_codey_request_config(
    config: &crate::config::CodeyConfig,
    optimization: &PromptOptimizationConfig,
    endpoint: local_router::RuntimeRouterEndpoint,
) -> prompt_optimization::ResolvedPromptOptimizationConfig {
    let mut request_headers = BTreeMap::new();
    request_headers.insert(local_router::ROUTER_AUTH_HEADER.to_string(), endpoint.token);
    let uses_official_account =
        codey_route_model_uses_official_account(config, &optimization.model);
    prompt_optimization::ResolvedPromptOptimizationConfig {
        base_url: endpoint.base_url,
        api_key: String::new(),
        request_headers,
        response_store: uses_official_account.then_some(false),
        // Codey routes always ask for a streamed Responses body: some
        // third-party relays only fill `output` for streamed requests and
        // answer non-streamed ones with a billed but empty result.
        response_stream: Some(true),
        response_omit_max_output_tokens: uses_official_account,
        model: optimization.model.clone(),
        upstream_protocol: crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string(),
        instruction: optimization.instruction.clone(),
    }
}

fn codey_route_model_uses_official_account(
    config: &crate::config::CodeyConfig,
    requested_model: &str,
) -> bool {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        return false;
    }

    let mut raw_match = None;
    for profile in &config.profiles {
        if !profile.enabled || (profile.official_account && !config.official_route_usable(profile))
        {
            continue;
        }
        let provider_id = profile.provider_id();
        let models = if profile.official_account {
            config.enabled_official_route_models(provider_id)
        } else {
            config.enabled_route_models(provider_id)
        };
        for model in models {
            if requested_model == local_router::model_alias(provider_id, &model) {
                return profile.official_account;
            }
            if requested_model == model {
                if raw_match.is_some() {
                    // The router also rejects an unqualified model that belongs
                    // to more than one route instead of guessing its identity.
                    return false;
                }
                raw_match = Some(profile.official_account);
            }
        }
    }
    raw_match.unwrap_or(false)
}

fn resolve_manual_request_config_at(
    optimization: &PromptOptimizationConfig,
    codex_home: &Path,
) -> Result<prompt_optimization::ResolvedPromptOptimizationConfig, String> {
    if optimization.api_key.trim().is_empty() {
        return Err("请先配置 API Key".to_string());
    }
    let mut resolved =
        prompt_optimization::ResolvedPromptOptimizationConfig::from_custom(optimization);
    if let Some(installation_id) = read_codex_installation_id(codex_home) {
        resolved
            .request_headers
            .insert(CODEX_INSTALLATION_ID_HEADER.to_string(), installation_id);
    }
    Ok(resolved)
}

fn read_codex_installation_id(codex_home: &Path) -> Option<String> {
    let value = fs::read_to_string(codex_home.join(CODEX_INSTALLATION_ID_FILE)).ok()?;
    Uuid::parse_str(value.trim())
        .ok()
        .map(|installation_id| installation_id.to_string())
}

pub async fn optimize_prompt_command(state: &Arc<AppState>, text: String) -> Result<Value, String> {
    let optimization = state.config.read().await.prompt_optimization.clone();
    if !optimization.enabled {
        return Err("提示词优化尚未启用，请先在 Codey 控制台开启".to_string());
    }
    let uses_codey_route = optimization.uses_codey_route();
    let request_config = resolve_request_config(state, &optimization).await?;
    let client = optimizer_client(uses_codey_route)?;
    match prompt_optimization::optimize_prompt_resolved(client, &request_config, &text).await {
        Ok(optimized) => Ok(json!({"optimized": optimized})),
        Err(error) => {
            error_log::record_failure(
                "prompt_optimization_failed",
                "optimize_prompt",
                error.clone(),
                json!({
                    "model": optimization.model.trim(),
                    "apiSource": "configured",
                }),
            );
            Err(error)
        }
    }
}

/// Fetches the model list advertised by the configured service for the
/// console picker. Accepts an unsaved draft like the connectivity test.
pub async fn fetch_prompt_optimization_models_command(
    state: &Arc<AppState>,
    draft: Option<PromptOptimizationConfig>,
) -> Result<Value, String> {
    let saved_optimization = state.config.read().await.prompt_optimization.clone();
    let mut optimization = draft.unwrap_or_else(|| saved_optimization.clone());
    optimization.merge_redacted_secrets(&saved_optimization);
    // 获取列表不需要预先选择模型，连接参数由后续请求流程校验。
    let uses_codey_route = optimization.uses_codey_route();
    let request_config = resolve_request_config(state, &optimization).await?;
    let client = optimizer_client(uses_codey_route)?;
    let models = prompt_optimization::fetch_models_resolved(client, &request_config).await;
    match models {
        Ok(models) => Ok(json!({"models": models})),
        Err(error) => {
            error_log::record_failure(
                "prompt_optimization_models_failed",
                "fetch_prompt_optimization_models",
                error.clone(),
                json!({ "apiSource": "configured" }),
            );
            Err(error)
        }
    }
}

/// Tests connectivity against the saved configuration, or against an
/// unsaved draft passed by the console. The compatibility merge still accepts
/// older redacted drafts before the request is sent.
pub async fn test_prompt_optimization_command(
    state: &Arc<AppState>,
    draft: Option<PromptOptimizationConfig>,
) -> Result<Value, String> {
    let saved_optimization = state.config.read().await.prompt_optimization.clone();
    let mut optimization = draft.unwrap_or_else(|| saved_optimization.clone());
    optimization.merge_redacted_secrets(&saved_optimization);
    optimization.validate()?;
    let uses_codey_route = optimization.uses_codey_route();
    let request_config = resolve_request_config(state, &optimization).await?;
    let client = optimizer_client(uses_codey_route)?;
    match prompt_optimization::test_configuration_resolved(client, &request_config).await {
        Ok(result) => Ok(json!({"status": "ok", "result": result})),
        Err(error) => {
            error_log::record_failure(
                "prompt_optimization_test_failed",
                "test_prompt_optimization",
                error.clone(),
                json!({
                    "model": optimization.model.trim(),
                    "apiSource": "configured",
                }),
            );
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetch_prompt_optimization_models_accepts_drafts_without_a_selected_model() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::time::{Duration, timeout};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0_u8; 1024];
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert!(read > 0, "模型列表请求不完整");
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
                assert!(request.contains("Bearer sk-test-model-list"));
                let body = r#"{"data":[{"id":"model-a"},{"id":"model-b"}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let saved = PromptOptimizationConfig {
            base_url: format!("http://{address}/v1"),
            api_key: "sk-test-model-list".to_string(),
            api_key_configured: true,
            model: "saved-model".to_string(),
            upstream_protocol: crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
            ..PromptOptimizationConfig::default()
        };
        let state = Arc::new(AppState {
            config: tokio::sync::RwLock::new(crate::config::CodeyConfig {
                prompt_optimization: saved.clone(),
                ..crate::config::CodeyConfig::default()
            }),
            ..AppState::default()
        });

        for enabled in [true, false] {
            let draft = PromptOptimizationConfig {
                enabled,
                api_key: String::new(),
                model: String::new(),
                ..saved.clone()
            };
            let result = timeout(
                Duration::from_secs(5),
                fetch_prompt_optimization_models_command(&state, Some(draft)),
            )
            .await
            .unwrap()
            .unwrap();

            assert_eq!(result, json!({"models": ["model-a", "model-b"]}));
            assert_eq!(state.config.read().await.prompt_optimization, saved);
        }
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn disabled_local_router_rejects_codey_route_optimization_before_runtime_lookup() {
        let config = crate::config::CodeyConfig {
            local_router_enabled: false,
            ..crate::config::CodeyConfig::default()
        };
        let state = Arc::new(AppState {
            config: tokio::sync::RwLock::new(config),
            ..AppState::default()
        });
        let optimization = PromptOptimizationConfig {
            mode: crate::config::PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE.to_string(),
            model: "relay/model".to_string(),
            ..PromptOptimizationConfig::default()
        };

        let error = resolve_request_config(&state, &optimization)
            .await
            .unwrap_err();

        assert!(error.contains("本地路由已关闭"));
    }

    #[test]
    fn codey_route_official_model_detection_matches_route_alias() {
        let mut official = crate::config::ProviderProfile::new("OpenAI 官方直登");
        official.id = crate::config::DERIVED_OFFICIAL_PROFILE_ID.to_string();
        official.source_provider_id = Some("openai".to_string());
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
        official.normalize();
        let mut relay = crate::config::ProviderProfile::new("Relay");
        relay.id = "relay-route".to_string();
        relay.base_url = "https://relay.example/v1".to_string();
        relay.normalize();
        let mut config = crate::config::CodeyConfig {
            profiles: vec![official, relay],
            official_account_available_this_launch: true,
            ..crate::config::CodeyConfig::default()
        }
        .normalize();
        config
            .declared_official_models_by_provider
            .insert("openai".to_string(), vec!["gpt-5.6-luna".to_string()]);
        config
            .selected_models_by_provider
            .insert("relay-route".to_string(), vec!["gpt-5.6-luna".to_string()]);

        assert!(codey_route_model_uses_official_account(
            &config,
            "openai/gpt-5.6-luna"
        ));
        assert!(!codey_route_model_uses_official_account(
            &config,
            "gpt-5.6-luna"
        ));
        assert!(!codey_route_model_uses_official_account(
            &config,
            "relay-route/gpt-5.6-luna"
        ));
    }

    #[test]
    fn codey_route_parameters_follow_selected_route_independently_of_default_auth() {
        let mut official = crate::config::ProviderProfile::new("官方账号2");
        official.id = "official-two".to_string();
        official.source_provider_id = Some("official-two".to_string());
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
        official.official_account_id = Some("account-two".to_string());
        official.normalize();
        let mut relay = crate::config::ProviderProfile::new("Relay");
        relay.id = "relay-route".to_string();
        relay.base_url = "https://relay.example/v1".to_string();
        relay.normalize();
        let mut config = crate::config::CodeyConfig {
            profiles: vec![official, relay],
            local_router_enabled: true,
            official_account_available_this_launch: false,
            ..crate::config::CodeyConfig::default()
        };
        config
            .declared_official_models_by_provider
            .insert("official-two".to_string(), vec!["gpt-5.6-luna".to_string()]);
        config
            .selected_models_by_provider
            .insert("relay-route".to_string(), vec!["gpt-5.6-luna".to_string()]);

        for default_auth_available in [false, true] {
            config.official_account_available_this_launch = default_auth_available;
            for requires_openai_auth in [false, true] {
                for (model, official) in [
                    ("official-two/gpt-5.6-luna", true),
                    ("relay-route/gpt-5.6-luna", false),
                    ("gpt-5.6-luna", false),
                    ("missing/model", false),
                    ("", false),
                ] {
                    let resolved = resolve_codey_request_config(
                        &config,
                        &PromptOptimizationConfig {
                            model: model.to_string(),
                            ..PromptOptimizationConfig::default()
                        },
                        local_router::RuntimeRouterEndpoint {
                            base_url: "http://127.0.0.1:12345/v1".to_string(),
                            token: "test-router-token".to_string(),
                            supports_websockets: false,
                            supports_remote_compaction: false,
                            requires_openai_auth,
                        },
                    );
                    assert_eq!(
                        resolved.response_store,
                        official.then_some(false),
                        "{model}"
                    );
                    assert_eq!(
                        resolved.response_omit_max_output_tokens, official,
                        "{model}"
                    );
                    assert_eq!(resolved.response_stream, Some(true));
                    assert_eq!(
                        resolved.request_headers,
                        BTreeMap::from([(
                            local_router::ROUTER_AUTH_HEADER.to_string(),
                            "test-router-token".to_string()
                        ),])
                    );
                }
            }
        }

        config.profiles[1].enabled = false;
        assert!(codey_route_model_uses_official_account(
            &config,
            "gpt-5.6-luna"
        ));
        config.profiles[0].enabled = false;
        assert!(!codey_route_model_uses_official_account(
            &config,
            "official-two/gpt-5.6-luna"
        ));
    }

    #[test]
    fn resolved_prompt_optimization_uses_codex_installation_id_header() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(CODEX_INSTALLATION_ID_FILE),
            " 49A95816-9EAD-4F14-B008-1D0CBAA3C328\n",
        )
        .unwrap();
        let optimization = PromptOptimizationConfig {
            api_key: "sk-test".to_string(),
            ..PromptOptimizationConfig::default()
        };

        let resolved = resolve_manual_request_config_at(&optimization, directory.path()).unwrap();

        assert_eq!(
            resolved
                .request_headers
                .get(CODEX_INSTALLATION_ID_HEADER)
                .map(String::as_str),
            Some("49a95816-9ead-4f14-b008-1d0cbaa3c328")
        );
        assert_eq!(resolved.response_store, None);
        assert_eq!(resolved.response_stream, None);
        assert!(!resolved.response_omit_max_output_tokens);
    }

    #[test]
    fn invalid_codex_installation_id_is_not_forwarded() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(CODEX_INSTALLATION_ID_FILE),
            "not-an-installation-id",
        )
        .unwrap();
        let optimization = PromptOptimizationConfig {
            api_key: "sk-test".to_string(),
            ..PromptOptimizationConfig::default()
        };

        let resolved = resolve_manual_request_config_at(&optimization, directory.path()).unwrap();

        assert!(
            !resolved
                .request_headers
                .contains_key(CODEX_INSTALLATION_ID_HEADER)
        );
    }
}
