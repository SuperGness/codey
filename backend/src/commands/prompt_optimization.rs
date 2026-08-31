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
const CODEX_INSTALLATION_ID_HEADER: &str = "x-codex-installation-id";
const CODEX_INSTALLATION_ID_FILE: &str = "installation_id";
const AUTHORIZATION_HEADER: &str = "authorization";
const CHATGPT_ACCOUNT_ID_HEADER: &str = "chatgpt-account-id";

fn optimizer_client() -> Result<&'static Client, String> {
    if let Some(client) = OPTIMIZER_CLIENT.get() {
        return Ok(client);
    }
    let client = prompt_optimization::optimizer_http_client()?;
    // Concurrent callers may build a duplicate client; the first successful
    // one wins and the rest reuse it.
    Ok(OPTIMIZER_CLIENT.get_or_init(|| client))
}

async fn resolve_request_config(
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
        let mut request_headers = std::collections::BTreeMap::new();
        request_headers.insert(local_router::ROUTER_AUTH_HEADER.to_string(), endpoint.token);
        let uses_official_account = endpoint.requires_openai_auth
            && codey_route_model_uses_official_account(&config, &optimization.model);
        if uses_official_account {
            request_headers.extend(read_official_auth_headers(codex_home())?);
        }
        return Ok(prompt_optimization::ResolvedPromptOptimizationConfig {
            base_url: endpoint.base_url,
            api_key: String::new(),
            request_headers,
            response_store: uses_official_account.then_some(false),
            response_stream: uses_official_account.then_some(true),
            response_omit_max_output_tokens: uses_official_account,
            model: optimization.model.clone(),
            upstream_protocol: crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string(),
            instruction: optimization.instruction.clone(),
        });
    }
    resolve_manual_request_config_at(optimization, codex_home())
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
        if profile.official_account && !config.official_account_available_this_launch {
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

fn read_official_auth_headers(codex_home: &Path) -> Result<BTreeMap<String, String>, String> {
    let auth = crate::account_usage::read_official_auth(&codex_home.join("auth.json"))
        .map_err(|error| format!("读取 Codex 官方账号登录态失败：{error}"))?;
    let mut headers = BTreeMap::new();
    headers.insert(
        AUTHORIZATION_HEADER.to_string(),
        format!("Bearer {}", auth.access_token),
    );
    if let Some(account_id) = auth.account_id {
        headers.insert(CHATGPT_ACCOUNT_ID_HEADER.to_string(), account_id);
    }
    Ok(headers)
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
    let config = state.config.read().await.clone();
    let optimization = config.prompt_optimization.clone();
    if !optimization.enabled {
        return Err("提示词优化尚未启用，请先在 Codey 控制台开启".to_string());
    }
    let request_config = resolve_request_config(state, &optimization).await?;
    let client = optimizer_client()?;
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
    let config = state.config.read().await.clone();
    let mut optimization = draft.unwrap_or_else(|| config.prompt_optimization.clone());
    optimization.merge_redacted_secrets(&config.prompt_optimization);
    optimization.validate()?;
    let request_config = resolve_request_config(state, &optimization).await?;
    let client = optimizer_client()?;
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
    let config = state.config.read().await.clone();
    let mut optimization = draft.unwrap_or_else(|| config.prompt_optimization.clone());
    optimization.merge_redacted_secrets(&config.prompt_optimization);
    optimization.validate()?;
    let request_config = resolve_request_config(state, &optimization).await?;
    let client = optimizer_client()?;
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
    fn official_auth_headers_are_loaded_from_codex_auth_json() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("auth.json"),
            serde_json::to_vec_pretty(&json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "access_token": "access-token",
                    "account_id": "account-123"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let headers = read_official_auth_headers(directory.path()).unwrap();

        assert_eq!(
            headers.get(AUTHORIZATION_HEADER).map(String::as_str),
            Some("Bearer access-token")
        );
        assert_eq!(
            headers.get(CHATGPT_ACCOUNT_ID_HEADER).map(String::as_str),
            Some("account-123")
        );
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
