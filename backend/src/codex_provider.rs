use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use codey_runtime_core::config_manager::ConfigManager;
use serde::Serialize;
use serde_json::Value;
use toml_edit::{DocumentMut, Item, TableLike};

use crate::codex_config::BUILTIN_OPENAI_PROVIDER_ID;
use crate::config::{CodeyConfig, DERIVED_OFFICIAL_PROFILE_ID, ProviderProfile};
use crate::official_accounts::{LaunchLoginResolution, OfficialAccountStore};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CurrentProvider {
    pub id: String,
    pub name: String,
    pub official: bool,
    pub supports_remote_compaction: bool,
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub changed: bool,
    pub provider: CurrentProvider,
}

struct ProviderRequestExtensions {
    api_key: Option<String>,
    headers: BTreeMap<String, String>,
}

struct LocalProviderSnapshot {
    provider: CurrentProvider,
    api_key: String,
    upstream_protocol: String,
    official_account_auth: OfficialAccountAuthProbe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OfficialAccountAuthProbe {
    Available(String),
    Unavailable(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfficialAccountProfileStatus {
    Available(ProviderProfile),
    Unavailable {
        reason: String,
    },
    Unknown {
        profile: ProviderProfile,
        reason: String,
    },
}

/// Decides whether the official route can be used for this launch. The
/// decision comes from Codey's own account store: the default account is
/// copied into the Codex home, and its presence makes the route available.
/// With no accounts stored, an existing ChatGPT login in the Codex home is
/// adopted once so earlier installs keep working.
pub fn current_official_account_profile_status_for_launch(
    codex_home: &Path,
    accounts: &OfficialAccountStore,
) -> Result<OfficialAccountProfileStatus> {
    let resolution = accounts.resolve_launch_login(codex_home);
    let snapshot = local_provider_with_auth_policy(codex_home, AuthProbePolicy::Lenient)?;
    let profile = official_profile_from_snapshot(&snapshot);
    let file_reason = match &snapshot.official_account_auth {
        OfficialAccountAuthProbe::Available(reason)
        | OfficialAccountAuthProbe::Unavailable(reason)
        | OfficialAccountAuthProbe::Unknown(reason) => reason.clone(),
    };
    Ok(match resolution {
        Ok(LaunchLoginResolution::Available { .. }) => {
            OfficialAccountProfileStatus::Available(profile)
        }
        Ok(LaunchLoginResolution::Unavailable { reason }) => {
            OfficialAccountProfileStatus::Unavailable {
                reason: format!("{reason}；文件凭据探针：{file_reason}"),
            }
        }
        Err(error) => OfficialAccountProfileStatus::Unknown {
            profile,
            reason: format!(
                "同步默认官方账号到 Codex 失败：{error:#}；文件凭据探针：{file_reason}"
            ),
        },
    })
}

fn official_profile_from_snapshot(snapshot: &LocalProviderSnapshot) -> ProviderProfile {
    if snapshot.provider.official {
        let provider_id = snapshot.provider.id.clone();
        let mut profile = profile_from_provider(
            &snapshot.provider,
            String::new(),
            &snapshot.upstream_protocol,
        );
        profile.id = DERIVED_OFFICIAL_PROFILE_ID.to_string();
        profile.source_provider_id = Some(provider_id);
        profile.normalize();
        return profile;
    }

    let mut profile = profile_from_provider(&builtin_official_provider(), String::new(), "");
    profile.id = DERIVED_OFFICIAL_PROFILE_ID.to_string();
    profile.source_provider_id = Some(BUILTIN_OPENAI_PROVIDER_ID.to_string());
    profile.normalize();
    profile
}

pub fn current_provider(codex_home: &Path) -> Result<CurrentProvider> {
    Ok(local_provider(codex_home)?.provider)
}

pub fn provider_model_fetch_profile(
    profile: &ProviderProfile,
    codex_home: &Path,
) -> Result<ProviderProfile> {
    let mut fetch_profile = profile.clone();
    if let Some(extensions) = local_provider_model_request_extensions(codex_home, profile)? {
        if let Some(api_key) = extensions.api_key {
            fetch_profile.api_key = api_key;
        }
        let mut headers = extensions.headers;
        for (name, value) in &profile.model_request_headers {
            headers.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
            headers.insert(name.clone(), value.clone());
        }
        fetch_profile.model_request_headers = headers;
    }
    Ok(fetch_profile)
}

fn local_provider_model_request_extensions(
    codex_home: &Path,
    profile: &ProviderProfile,
) -> Result<Option<ProviderRequestExtensions>> {
    let config_path = codex_home.join("config.toml");
    let snapshot = ConfigManager::new(&config_path).load()?;
    if !snapshot.exists() {
        return Ok(None);
    }
    let document = snapshot.document();
    let provider_id = active_provider_id(document);
    if provider_id != profile.provider_id() {
        return Ok(None);
    }
    let provider = provider_table(document, provider_id);
    Ok(provider.map(|provider| ProviderRequestExtensions {
        api_key: provider_config_api_key(document, Some(provider)),
        headers: provider_model_request_headers(provider),
    }))
}

#[cfg(test)]
pub fn sync_current_provider(
    config: &CodeyConfig,
    codex_home: &Path,
) -> Result<(CodeyConfig, ProviderStatus)> {
    let snapshot = local_provider(codex_home)?;
    sync_provider_profile(
        config,
        snapshot.provider,
        snapshot.api_key,
        &snapshot.upstream_protocol,
    )
}

pub fn sync_current_third_party_provider(
    config: &CodeyConfig,
    codex_home: &Path,
) -> Result<(CodeyConfig, ProviderStatus)> {
    let snapshot = local_provider(codex_home)?;
    if snapshot.provider.official {
        bail!("当前 Codex 配置是官方账号线路，不自动导入为第三方线路");
    }
    sync_provider_profile(
        config,
        snapshot.provider,
        snapshot.api_key,
        &snapshot.upstream_protocol,
    )
}

fn sync_provider_profile(
    config: &CodeyConfig,
    provider: CurrentProvider,
    api_key: String,
    upstream_protocol: &str,
) -> Result<(CodeyConfig, ProviderStatus)> {
    let profile = profile_from_provider(&provider, api_key, upstream_protocol);
    let mut next = config.clone();
    let imported_id = profile.id.clone();
    let imported_provider_id = profile.provider_id().to_string();
    let mut active_profile_id = imported_id.clone();
    let replace_placeholder =
        next.profiles.len() == 1 && next.profiles[0].is_unconfigured_default();
    if replace_placeholder {
        let placeholder_provider_id = next.profiles[0].provider_id().to_string();
        next.profiles = vec![profile];
        next.selected_models_by_provider
            .remove(&placeholder_provider_id);
        next.model_reasoning_efforts_by_provider
            .remove(&placeholder_provider_id);
        next.model_context_by_provider
            .remove(&placeholder_provider_id);
        next.manual_third_party_models_by_provider
            .remove(&placeholder_provider_id);
        next.declared_official_models_by_provider
            .remove(&placeholder_provider_id);
        next.upstream_models_by_provider
            .remove(&placeholder_provider_id);
    } else if let Some(existing) = next.profiles.iter_mut().find(|existing| {
        existing.provider_id() == imported_provider_id || existing.id == imported_id
    }) {
        // Keep the Codey UI identity stable when a previously imported route
        // has a different runtime provider id.
        active_profile_id = existing.id.clone();
        let mut replacement = profile;
        replacement.enabled = existing.enabled;
        replacement.short_name.clone_from(&existing.short_name);
        if replacement.id != active_profile_id {
            replacement.id = active_profile_id.clone();
            replacement.source_provider_id = Some(imported_provider_id);
        }
        *existing = replacement;
    } else {
        next.profiles.push(profile);
    }
    next.active_profile_id = active_profile_id;
    next.initial_route_import_completed = true;
    next = next.normalize();
    // One-shot launch flags must not bump `settings_revision`.
    let changed = serde_json::to_value(&next)? != serde_json::to_value(config)?;
    if changed {
        next.settings_revision = config.settings_revision.saturating_add(1);
    }
    Ok((next, ProviderStatus { changed, provider }))
}

pub fn status_from_config(config: &CodeyConfig) -> ProviderStatus {
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == config.active_profile_id)
        .or_else(|| config.profiles.first());
    let provider = profile
        .map(|profile| CurrentProvider {
            id: profile.id.clone(),
            name: profile.name.clone(),
            official: profile.official_account,
            supports_remote_compaction: profile.supports_remote_compaction,
            base_url: profile.base_url.clone(),
        })
        .unwrap_or_else(|| CurrentProvider {
            id: BUILTIN_OPENAI_PROVIDER_ID.to_string(),
            name: "OpenAI 官方直登".to_string(),
            official: true,
            supports_remote_compaction: true,
            base_url: String::new(),
        });
    ProviderStatus {
        changed: false,
        provider,
    }
}

fn profile_from_provider(
    provider: &CurrentProvider,
    api_key: String,
    upstream_protocol: &str,
) -> ProviderProfile {
    ProviderProfile {
        id: provider.id.clone(),
        enabled: true,
        name: provider.name.clone(),
        short_name: String::new(),
        base_url: provider.base_url.clone(),
        api_key,
        upstream_protocol: if provider.official {
            crate::config::UPSTREAM_PROTOCOL_OFFICIAL.to_string()
        } else {
            upstream_protocol.to_string()
        },
        auth_mode: if provider.official {
            crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.to_string()
        } else {
            crate::config::AUTH_MODE_API_KEY.to_string()
        },
        api_key_configured: !provider.official,
        clear_api_key: false,
        model_request_headers: BTreeMap::new(),
        upstream_proxy: String::new(),
        source_provider_id: None,
        official_account: provider.official,
        supports_remote_compaction: provider.supports_remote_compaction,
        supports_websockets: provider.official,
        supports_native_web_search: provider.official,
        supports_auto_review: provider.official,
    }
}

fn local_provider(codex_home: &Path) -> Result<LocalProviderSnapshot> {
    local_provider_with_auth_policy(codex_home, AuthProbePolicy::Strict)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthProbePolicy {
    Strict,
    Lenient,
}

struct AuthProbe {
    value: Option<Value>,
    status: OfficialAccountAuthProbe,
}

fn local_provider_with_auth_policy(
    codex_home: &Path,
    auth_policy: AuthProbePolicy,
) -> Result<LocalProviderSnapshot> {
    let config_path = codex_home.join("config.toml");
    let config = ConfigManager::new(&config_path).load()?;
    let document = config.document();
    let provider_id = active_provider_id(document);
    let table = provider_table(document, provider_id);
    let mut base_url = table
        .and_then(|provider| provider.get("base_url"))
        .and_then(Item::as_str)
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    let name = table
        .and_then(|provider| provider.get("name"))
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(provider_id);
    let wire_api = table
        .and_then(|provider| provider.get("wire_api"))
        .and_then(Item::as_str)
        .unwrap_or("responses");
    let upstream_protocol = upstream_protocol_from_wire_api(wire_api)?;
    let auth_path = codex_home.join("auth.json");
    let auth_store = document
        .get("cli_auth_credentials_store")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("auto");
    let auth = read_auth_probe(&auth_path, auth_store, auth_policy)?;
    let auth_mode = auth
        .value
        .as_ref()
        .and_then(|auth| auth.get("auth_mode"))
        .and_then(Value::as_str);
    let auth_api_key = auth
        .value
        .as_ref()
        .and_then(|auth| auth.get("OPENAI_API_KEY"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let config_api_key = provider_config_api_key(document, table);
    let has_provider_scoped_api_key = config_api_key.is_some()
        || PROVIDER_KEYS.iter().chain(PROVIDER_ENV_KEYS).any(|key| {
            table
                .and_then(|provider| provider.get(key))
                .and_then(Item::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
        || ["http_headers", "env_http_headers"].iter().any(|key| {
            table
                .and_then(|provider| provider.get(key))
                .and_then(Item::as_table_like)
                .is_some_and(|headers| {
                    headers
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                })
        });
    let official_endpoint = (base_url.is_empty() && provider_id == BUILTIN_OPENAI_PROVIDER_ID)
        || is_official_base_url(&base_url);
    // A provider-scoped token describes the active route and must win over a
    // long-lived auth.json login retained alongside it.
    let api_key = config_api_key
        .or_else(|| auth_api_key.map(ToString::to_string))
        .unwrap_or_default();
    let official = official_endpoint
        && !has_provider_scoped_api_key
        && table
            .and_then(|provider| provider.get("requires_openai_auth"))
            .and_then(Item::as_bool)
            != Some(false)
        && matches!(auth_mode, None | Some("chatgpt"))
        && api_key.is_empty();
    if !official && base_url.is_empty() {
        base_url = "https://api.openai.com/v1".to_string();
    }
    Ok(LocalProviderSnapshot {
        provider: CurrentProvider {
            id: provider_id.to_string(),
            name: if official {
                "OpenAI 官方直登".to_string()
            } else if name == BUILTIN_OPENAI_PROVIDER_ID {
                "OpenAI API".to_string()
            } else {
                name.to_string()
            },
            official,
            supports_remote_compaction: official || name == "OpenAI",
            base_url,
        },
        api_key: if official { String::new() } else { api_key },
        upstream_protocol: if official {
            crate::config::UPSTREAM_PROTOCOL_OFFICIAL.to_string()
        } else {
            upstream_protocol.to_string()
        },
        official_account_auth: auth.status,
    })
}

fn read_auth_probe(
    auth_path: &Path,
    auth_store: &str,
    policy: AuthProbePolicy,
) -> Result<AuthProbe> {
    let missing_auth_status = if auth_store.eq_ignore_ascii_case("file") {
        OfficialAccountAuthProbe::Unavailable(format!(
            "凭据存储策略为 file，但 auth.json 不存在：{}",
            auth_path.display()
        ))
    } else {
        OfficialAccountAuthProbe::Unknown(format!(
            "未找到 Codex auth.json，当前凭据存储为 {auth_store}，可能由系统凭据存储接管：{}",
            auth_path.display()
        ))
    };
    let bytes = match fs::read(auth_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuthProbe {
                value: None,
                status: missing_auth_status,
            });
        }
        Err(error) if policy == AuthProbePolicy::Lenient => {
            return Ok(AuthProbe {
                value: None,
                status: OfficialAccountAuthProbe::Unknown(format!(
                    "读取 Codex auth.json 失败，无法确认官方登录状态：{}：{error}",
                    auth_path.display()
                )),
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取本地 Codex 认证失败：{}", auth_path.display()));
        }
    };
    let value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => value,
        Err(error) if policy == AuthProbePolicy::Lenient => {
            return Ok(AuthProbe {
                value: None,
                status: OfficialAccountAuthProbe::Unknown(format!(
                    "解析 Codex auth.json 失败，无法确认官方登录状态：{}：{error}",
                    auth_path.display()
                )),
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("解析本地 Codex 认证失败：{}", auth_path.display()));
        }
    };
    let auth_summary = auth_file_safe_summary(&value);
    let status = if auth_has_chatgpt_tokens(&value) {
        OfficialAccountAuthProbe::Available(format!(
            "auth.json 包含可用的 ChatGPT 登录字段（{auth_summary}）：{}",
            auth_path.display()
        ))
    } else if auth_store.eq_ignore_ascii_case("file") {
        OfficialAccountAuthProbe::Unavailable(format!(
            "凭据存储策略为 file，但 auth.json 未包含可用的 ChatGPT 登录字段（{auth_summary}）：{}",
            auth_path.display()
        ))
    } else {
        OfficialAccountAuthProbe::Unknown(format!(
            "Codex auth.json 未包含 ChatGPT token（{auth_summary}），当前凭据存储为 {auth_store}，可能由系统凭据存储接管：{}",
            auth_path.display()
        ))
    };
    Ok(AuthProbe {
        value: Some(value),
        status,
    })
}

fn builtin_official_provider() -> CurrentProvider {
    CurrentProvider {
        id: BUILTIN_OPENAI_PROVIDER_ID.to_string(),
        name: "OpenAI 官方直登".to_string(),
        official: true,
        supports_remote_compaction: true,
        base_url: String::new(),
    }
}

fn active_provider_id(document: &DocumentMut) -> &str {
    let profile = document
        .get("profile")
        .and_then(Item::as_str)
        .and_then(|name| document.get("profiles")?.get(name));
    profile
        .and_then(|profile| profile.get("model_provider"))
        .or_else(|| document.get("model_provider"))
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(BUILTIN_OPENAI_PROVIDER_ID)
}

fn provider_table<'a>(document: &'a DocumentMut, provider_id: &str) -> Option<&'a dyn TableLike> {
    document
        .get("model_providers")
        .and_then(Item::as_table_like)
        .and_then(|providers| providers.get(provider_id))
        .and_then(Item::as_table_like)
}

fn auth_has_chatgpt_tokens(auth: &Value) -> bool {
    (matches!(auth.get("auth_mode"), None | Some(Value::Null))
        || auth.get("auth_mode").and_then(Value::as_str) == Some("chatgpt"))
        && auth
            .get("tokens")
            .and_then(Value::as_object)
            .is_some_and(|tokens| {
                ["access_token", "id_token", "refresh_token"]
                    .iter()
                    .any(|name| {
                        tokens
                            .get(*name)
                            .and_then(Value::as_str)
                            .is_some_and(|token| !token.trim().is_empty())
                    })
            })
}

fn auth_file_safe_summary(auth: &Value) -> String {
    let auth_mode = match auth
        .get("auth_mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("chatgpt") => "chatgpt",
        Some("api") | Some("api_key") | Some("apikey") => "api_key",
        Some(_) => "other",
        None => "missing",
    };
    let token_fields = auth
        .get("tokens")
        .and_then(Value::as_object)
        .map(|tokens| {
            ["access_token", "id_token", "refresh_token"]
                .into_iter()
                .filter(|name| {
                    tokens
                        .get(*name)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let openai_api_key_present = auth
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    format!(
        "authMode={auth_mode}, chatgptTokenFields={token_fields:?}, openaiApiKeyPresent={openai_api_key_present}"
    )
}

fn provider_config_api_key(
    document: &DocumentMut,
    provider: Option<&dyn TableLike>,
) -> Option<String> {
    provider_config_api_key_with_env(document, provider, &|name| std::env::var(name).ok())
}

const PROVIDER_KEYS: &[&str] = &[
    "experimental_bearer_token",
    "api_key",
    "apikey",
    "bearer_token",
    "token",
];
const PROVIDER_ENV_KEYS: &[&str] = &[
    "env_key",
    "api_key_env",
    "api_key_env_var",
    "key_env",
    "bearer_token_env",
];

fn provider_config_api_key_with_env(
    document: &DocumentMut,
    provider: Option<&dyn TableLike>,
    env_value: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    PROVIDER_KEYS
        .iter()
        .find_map(|key| {
            provider
                .and_then(|provider| provider.get(key))
                .and_then(Item::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            PROVIDER_ENV_KEYS.iter().find_map(|key| {
                let name = provider
                    .and_then(|provider| provider.get(key))
                    .and_then(Item::as_str)?
                    .trim();
                if name.is_empty() {
                    return None;
                }
                env_value(name)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
        })
        .or_else(|| {
            document
                .get("experimental_bearer_token")
                .and_then(Item::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
}

fn provider_model_request_headers(provider: &dyn TableLike) -> BTreeMap<String, String> {
    provider_model_request_headers_with_env(provider, &|name| std::env::var(name).ok())
}

fn provider_model_request_headers_with_env(
    provider: &dyn TableLike,
    env_value: &impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    if let Some(configured) = provider.get("http_headers").and_then(Item::as_table_like) {
        for (name, item) in configured.iter() {
            if let Some(value) = item.as_str() {
                insert_model_request_header(&mut headers, name, value);
            }
        }
    }
    if let Some(configured) = provider
        .get("env_http_headers")
        .and_then(Item::as_table_like)
    {
        for (name, item) in configured.iter() {
            let Some(env_name) = item.as_str().map(str::trim).filter(|name| !name.is_empty())
            else {
                continue;
            };
            let Some(value) = env_value(env_name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            insert_model_request_header(&mut headers, name, &value);
        }
    }
    headers
}

fn insert_model_request_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    let name = name.trim();
    if name.is_empty() || (name.eq_ignore_ascii_case("authorization") && value.trim().is_empty()) {
        return;
    }
    if let Some(existing) = headers
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
    headers.insert(name.to_string(), value.to_string());
}

pub(crate) fn is_official_base_url(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("chatgpt.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.path().trim_end_matches('/') == "/backend-api/codex"
        && url.query().is_none()
        && url.fragment().is_none()
}

fn upstream_protocol_from_wire_api(value: &str) -> Result<&'static str> {
    let value = value.trim().to_ascii_lowercase();
    if value.contains("anthropic") || value == "messages" || value.ends_with("/messages") {
        return Ok(crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES);
    }
    if value.contains("chat") {
        return Ok(crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS);
    }
    if value.is_empty() || value.contains("response") {
        return Ok(crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES);
    }
    bail!("Codex Provider 使用了 Codey 不支持的 wire_api：{value}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use tempfile::TempDir;
    use toml_edit::DocumentMut;

    fn write_config(home: &Path, contents: &str) {
        fs::write(home.join("config.toml"), contents).unwrap();
    }

    fn write_auth(home: &Path, value: Value) {
        fs::write(
            home.join("auth.json"),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }

    fn third_party_config(wire_api: &str) -> String {
        format!(
            r#"model_provider = "relay"

[model_providers.relay]
name = "Relay"
base_url = "https://relay.example/v1"
wire_api = "{wire_api}"
experimental_bearer_token = "sk-relay"
"#,
        )
    }

    fn empty_store() -> (TempDir, OfficialAccountStore) {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join("official-accounts"));
        (dir, store)
    }

    fn status_with_empty_store(home: &Path) -> Result<OfficialAccountProfileStatus> {
        let (_dir, store) = empty_store();
        current_official_account_profile_status_for_launch(home, &store)
    }

    #[test]
    fn rejects_malformed_codex_files() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), "not = [valid");
        assert!(current_provider(home.path()).is_err());

        write_config(home.path(), "");
        fs::write(home.path().join("auth.json"), b"{").unwrap();
        assert!(current_provider(home.path()).is_err());
    }

    #[test]
    fn imports_supported_wire_protocols() {
        for (wire_api, expected) in [
            (
                "chat_completions",
                crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            ),
            (
                "anthropic/messages",
                crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
            ),
            (
                "responses",
                crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
            ),
        ] {
            let home = TempDir::new().unwrap();
            write_config(home.path(), &third_party_config(wire_api));
            let (config, status) =
                sync_current_third_party_provider(&CodeyConfig::default(), home.path()).unwrap();
            assert_eq!(config.profiles[0].upstream_protocol, expected);
            assert!(!config.profiles[0].supports_native_web_search);
            assert_eq!(status.provider.id, "relay");
        }
    }

    #[test]
    fn official_capability_follows_the_default_account() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), "");
        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "access_token": "token" }
            }),
        );
        let (_dir, store) = empty_store();
        let OfficialAccountProfileStatus::Available(profile) =
            current_official_account_profile_status_for_launch(home.path(), &store).unwrap()
        else {
            panic!("an existing ChatGPT login should be adopted as the default account");
        };
        assert!(profile.official_account);
        assert!(profile.supports_websockets);
        assert!(profile.supports_native_web_search);
        assert_eq!(profile.provider_id(), "openai");
        assert!(profile.api_key.is_empty());
        assert!(store.default_account_id().unwrap().is_some());

        // The default account is restored into the Codex home on every launch.
        fs::remove_file(home.path().join("auth.json")).unwrap();
        assert!(matches!(
            current_official_account_profile_status_for_launch(home.path(), &store).unwrap(),
            OfficialAccountProfileStatus::Available(_)
        ));
        assert!(home.path().join("auth.json").is_file());

        // Without any stored account the route is unavailable, whatever the
        // credential store setting says.
        fs::remove_file(home.path().join("auth.json")).unwrap();
        write_config(home.path(), r#"cli_auth_credentials_store = "file""#);
        let OfficialAccountProfileStatus::Unavailable { reason } =
            status_with_empty_store(home.path()).unwrap()
        else {
            panic!("no stored account should be unavailable");
        };
        assert!(reason.contains("没有设为默认的官方账号"));
        assert!(reason.contains("auth.json 不存在"));
    }

    #[test]
    fn legacy_chatgpt_tokens_without_auth_mode_are_adopted() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), "");
        let mut auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": { "access_token": "legacy-token" }
        });
        for mode in [None, Some(Value::Null), Some(serde_json::json!("chatgpt"))] {
            if let Some(mode) = mode {
                auth["auth_mode"] = mode;
            }
            write_auth(home.path(), auth.clone());
            assert!(matches!(
                status_with_empty_store(home.path()).unwrap(),
                OfficialAccountProfileStatus::Available(_)
            ));
        }
        for mode in [
            serde_json::json!("apikey"),
            serde_json::json!("other"),
            serde_json::json!(42),
        ] {
            auth["auth_mode"] = mode;
            assert!(!auth_has_chatgpt_tokens(&auth));
        }
        assert!(!auth_has_chatgpt_tokens(&serde_json::json!({
            "tokens": { "access_token": "  ", "refresh_token": null }
        })));
    }

    #[test]
    fn missing_login_is_unavailable_regardless_of_credential_store() {
        for store_kind in ["file", "auto", "keyring"] {
            let home = TempDir::new().unwrap();
            write_config(
                home.path(),
                &format!(r#"cli_auth_credentials_store = "{store_kind}""#),
            );
            let OfficialAccountProfileStatus::Unavailable { reason } =
                status_with_empty_store(home.path()).unwrap()
            else {
                panic!("no account and no login should be unavailable for {store_kind}");
            };
            assert!(reason.contains("没有设为默认的官方账号"));
            assert!(reason.contains("auth.json"));
        }
    }

    #[test]
    fn auth_file_summary_reports_presence_without_secret_values() {
        let summary = auth_file_safe_summary(&serde_json::json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": "sk-private-api-key",
            "tokens": {
                "access_token": "private-access-token",
                "refresh_token": "private-refresh-token"
            }
        }));

        assert!(summary.contains("authMode=chatgpt"));
        assert!(summary.contains("access_token"));
        assert!(summary.contains("refresh_token"));
        assert!(summary.contains("openaiApiKeyPresent=true"));
        assert!(!summary.contains("sk-private-api-key"));
        assert!(!summary.contains("private-access-token"));
        assert!(!summary.contains("private-refresh-token"));

        let unknown_mode = auth_file_safe_summary(&serde_json::json!({
            "auth_mode": "private-custom-auth-mode"
        }));
        assert!(unknown_mode.contains("authMode=other"));
        assert!(!unknown_mode.contains("private-custom-auth-mode"));
    }

    #[test]
    fn malformed_auth_json_is_unknown_for_launch_but_strict_provider_reads_still_fail() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), "");
        fs::write(home.path().join("auth.json"), b"{").unwrap();

        let status = status_with_empty_store(home.path()).unwrap();
        assert!(matches!(
            status,
            OfficialAccountProfileStatus::Unknown { .. }
        ));
        assert!(current_provider(home.path()).is_err());
    }

    #[test]
    fn retained_chatgpt_login_next_to_third_party_provider_is_adopted() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), &third_party_config("responses"));
        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "access_token": "retained-token" }
            }),
        );

        let OfficialAccountProfileStatus::Available(profile) =
            status_with_empty_store(home.path()).unwrap()
        else {
            panic!("a retained ChatGPT login should become the default account");
        };
        assert!(profile.official_account);
        assert!(profile.supports_websockets);
        assert_eq!(profile.provider_id(), "openai");
        assert!(profile.api_key.is_empty());

        let current = current_provider(home.path()).unwrap();
        assert!(!current.official);
        assert_eq!(current.id, "relay");
    }

    #[test]
    fn scoped_api_key_on_official_endpoint_stays_separate_from_chatgpt_login() {
        let home = TempDir::new().unwrap();
        write_config(
            home.path(),
            r#"model_provider = "relay"

[model_providers.relay]
name = "OpenAI API"
base_url = "https://api.openai.com/v1"
wire_api = "responses"
experimental_bearer_token = "sk-relay"
"#,
        );
        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "access_token": "retained-token" }
            }),
        );

        assert!(matches!(
            status_with_empty_store(home.path()).unwrap(),
            OfficialAccountProfileStatus::Available(_)
        ));

        let (imported, status) =
            sync_current_third_party_provider(&CodeyConfig::default(), home.path()).unwrap();
        assert!(!status.provider.official);
        assert_eq!(status.provider.id, "relay");
        assert_eq!(imported.profiles[0].api_key, "sk-relay");
        assert!(!imported.profiles[0].official_account);
    }

    #[test]
    fn provider_token_wins_over_retained_auth_key() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), &third_party_config("responses"));
        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": "stale-auth-key",
                "tokens": { "access_token": "retained-token" }
            }),
        );
        let (config, _) = sync_current_provider(&CodeyConfig::default(), home.path()).unwrap();
        assert_eq!(config.profiles[0].api_key, "sk-relay");
        assert!(!config.profiles[0].official_account);
    }

    #[test]
    fn official_route_detection_rejects_api_credentials_and_lookalike_urls() {
        let home = TempDir::new().unwrap();
        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "access_token": "retained-token" }
            }),
        );
        for base_url in [
            "https://api.openai.com/v1",
            "https://api.openai.com.relay.example/v1",
            "https://relay.example/api.openai.com",
            "https://relay.example/?upstream=chatgpt.com/backend-api/codex",
            "https://chatgpt.com@relay.example/backend-api/codex",
            "http://chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex-proxy",
            "https://chatgpt.com:8443/backend-api/codex",
        ] {
            write_config(
                home.path(),
                &format!("[model_providers.openai]\nbase_url = {base_url:?}\n"),
            );
            assert!(
                !current_provider(home.path()).unwrap().official,
                "{base_url}"
            );
            // The stored ChatGPT login stays usable as an account even when
            // the Codex config points the provider elsewhere.
            assert!(matches!(
                status_with_empty_store(home.path()).unwrap(),
                OfficialAccountProfileStatus::Available(_)
            ));
        }
        for extra in [
            "env_key = 'CODEY_TEST_UNSET_OFFICIAL_AUTH_KEY'",
            "requires_openai_auth = false",
            "http_headers = { Authorization = 'Bearer custom-token' }",
            "env_http_headers = { Authorization = 'CODEY_TEST_UNSET_OFFICIAL_AUTH_KEY' }",
        ] {
            write_config(home.path(), &format!("[model_providers.openai]\n{extra}\n"));
            assert!(!current_provider(home.path()).unwrap().official, "{extra}");
        }
        write_config(home.path(), "model_provider = 'relay'");
        assert!(!current_provider(home.path()).unwrap().official);
        write_config(
            home.path(),
            "profile = 'relay-profile'\n[profiles.relay-profile]\nmodel_provider = 'relay'",
        );
        assert_eq!(current_provider(home.path()).unwrap().id, "relay");
        assert!(!current_provider(home.path()).unwrap().official);
        for config in [
            "",
            "[model_providers.openai]\nbase_url = 'https://chatgpt.com/backend-api/codex/'",
        ] {
            write_config(home.path(), config);
            assert!(current_provider(home.path()).unwrap().official);
        }
        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": "api-key",
                "tokens": { "access_token": "retained-token" }
            }),
        );
        assert!(!current_provider(home.path()).unwrap().official);
    }

    #[test]
    fn synchronization_upserts_without_removing_saved_routes() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), &third_party_config("responses"));
        let mut saved = ProviderProfile::new("Saved");
        saved.id = "saved".into();
        saved.base_url = "https://saved.example/v1".into();
        saved.api_key = "sk-saved".into();
        saved.normalize();
        let config = CodeyConfig {
            profiles: vec![saved],
            active_profile_id: "saved".into(),
            initial_route_import_completed: true,
            ..CodeyConfig::default()
        };
        let (synced, _) = sync_current_third_party_provider(&config, home.path()).unwrap();
        assert_eq!(synced.profiles.len(), 2);
        assert!(synced.profiles.iter().any(|profile| profile.id == "saved"));
        assert!(synced.profiles.iter().any(|profile| profile.id == "relay"));
    }

    #[test]
    fn synchronization_preserves_an_existing_route_short_name() {
        let mut saved = ProviderProfile::new("Relay");
        saved.id = "relay".into();
        saved.short_name = "中".into();
        saved.base_url = "https://old.example/v1".into();
        saved.api_key = "old-key".into();
        saved.normalize();
        let config = CodeyConfig {
            profiles: vec![saved],
            active_profile_id: "relay".into(),
            initial_route_import_completed: true,
            ..CodeyConfig::default()
        };

        let (synced, _) = sync_provider_profile(
            &config,
            CurrentProvider {
                id: "relay".into(),
                name: "Relay Updated".into(),
                official: false,
                supports_remote_compaction: false,
                base_url: "https://new.example/v1".into(),
            },
            "new-key".into(),
            crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        )
        .unwrap();

        assert_eq!(synced.profiles[0].short_name, "中");
        assert_eq!(synced.profiles[0].name, "Relay Updated");
    }

    #[test]
    fn model_fetch_uses_active_provider_key_and_headers() {
        let home = TempDir::new().unwrap();
        write_config(
            home.path(),
            r#"model_provider = "relay"

[model_providers.relay]
name = "Relay"
base_url = "https://relay.example/v1"
wire_api = "responses"
experimental_bearer_token = "fresh-key"
http_headers = { X-Static = "static-value" }
env_http_headers = { X-Dynamic = "DYNAMIC_HEADER" }
"#,
        );
        let mut profile = ProviderProfile::new("Relay");
        profile.id = "relay".into();
        profile.base_url = "https://relay.example/v1".into();
        profile.api_key = "old-key".into();

        let document = DocumentMut::from_str(
            r#"http_headers = { X-Static = "static-value" }
env_http_headers = { X-Dynamic = "DYNAMIC_HEADER" }
"#,
        )
        .unwrap();
        let table = document.as_table();
        let headers = provider_model_request_headers_with_env(table, &|name| {
            (name == "DYNAMIC_HEADER").then(|| "dynamic-value".to_string())
        });
        assert_eq!(headers["X-Static"], "static-value");
        assert_eq!(headers["X-Dynamic"], "dynamic-value");

        let fetch = provider_model_fetch_profile(&profile, home.path()).unwrap();
        assert_eq!(fetch.api_key, "fresh-key");
        assert_eq!(fetch.model_request_headers["X-Static"], "static-value");

        profile
            .model_request_headers
            .insert("x-static".into(), String::new());
        profile
            .model_request_headers
            .insert("X-Saved".into(), "saved".into());
        let fetch = provider_model_fetch_profile(&profile, home.path()).unwrap();
        assert_eq!(fetch.model_request_headers["x-static"], "");
        assert!(!fetch.model_request_headers.contains_key("X-Static"));
        assert_eq!(fetch.model_request_headers["X-Saved"], "saved");
    }

    #[test]
    fn environment_provider_keys_are_supported() {
        let document = DocumentMut::from_str(
            r#"[model_providers.relay]
env_key = "RELAY_TOKEN"
"#,
        )
        .unwrap();
        let provider = provider_table(&document, "relay").unwrap();
        let key = provider_config_api_key_with_env(&document, Some(provider), &|name| {
            (name == "RELAY_TOKEN").then(|| "env-secret".to_string())
        });
        assert_eq!(key.as_deref(), Some("env-secret"));
    }
}
