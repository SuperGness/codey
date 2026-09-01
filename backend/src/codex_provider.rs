use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use anyhow::{Context, Result, bail};
use codey_runtime_core::app_paths::{codex_runtime_executable, resolve_codex_app_dir_with_saved};
use codey_runtime_core::config_manager::ConfigManager;
use serde::Serialize;
use serde_json::Value;
use toml_edit::{DocumentMut, Item, TableLike};

use crate::codex_config::BUILTIN_OPENAI_PROVIDER_ID;
use crate::config::{CodeyConfig, DERIVED_OFFICIAL_PROFILE_ID, ProviderProfile};

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
enum NativeLoginStatus {
    ChatGpt,
    NotLoggedIn(String),
    ApiKey(String),
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

pub fn current_official_account_profile_status_for_launch(
    codex_home: &Path,
    configured_codex_app_path: &str,
) -> Result<OfficialAccountProfileStatus> {
    let executable = resolve_native_status_executable(configured_codex_app_path);
    current_official_account_profile_status_with_probe(codex_home, |home| {
        native_login_status_with_path_fallback(home, &executable)
    })
}

fn current_official_account_profile_status_with_probe(
    codex_home: &Path,
    native_probe: impl FnOnce(&Path) -> NativeLoginStatus,
) -> Result<OfficialAccountProfileStatus> {
    let mut snapshot = local_provider_with_auth_policy(codex_home, AuthProbePolicy::Lenient)?;
    snapshot.official_account_auth =
        official_auth_probe_from_native(snapshot.official_account_auth, native_probe(codex_home));
    let profile = official_profile_from_snapshot(&snapshot);
    Ok(match snapshot.official_account_auth {
        OfficialAccountAuthProbe::Available(_) => OfficialAccountProfileStatus::Available(profile),
        OfficialAccountAuthProbe::Unavailable(reason) => {
            OfficialAccountProfileStatus::Unavailable { reason }
        }
        OfficialAccountAuthProbe::Unknown(reason) => {
            OfficialAccountProfileStatus::Unknown { profile, reason }
        }
    })
}

fn official_auth_probe_from_native(
    file_probe: OfficialAccountAuthProbe,
    native_status: NativeLoginStatus,
) -> OfficialAccountAuthProbe {
    match native_status {
        NativeLoginStatus::ChatGpt => OfficialAccountAuthProbe::Available(
            "Codex 原生认证探针确认当前使用 ChatGPT 登录".to_string(),
        ),
        NativeLoginStatus::NotLoggedIn(reason) | NativeLoginStatus::ApiKey(reason) => {
            let file_reason = match file_probe {
                OfficialAccountAuthProbe::Available(reason)
                | OfficialAccountAuthProbe::Unavailable(reason)
                | OfficialAccountAuthProbe::Unknown(reason) => reason,
            };
            OfficialAccountAuthProbe::Unavailable(format!("{reason}；文件凭据探针：{file_reason}"))
        }
        NativeLoginStatus::Unknown(reason) => match file_probe {
            OfficialAccountAuthProbe::Available(file_reason) => {
                OfficialAccountAuthProbe::Available(format!("{reason}；{file_reason}"))
            }
            OfficialAccountAuthProbe::Unavailable(file_reason) => {
                OfficialAccountAuthProbe::Unavailable(format!("{reason}；{file_reason}"))
            }
            OfficialAccountAuthProbe::Unknown(file_reason) => {
                OfficialAccountAuthProbe::Unknown(format!("{reason}；{file_reason}"))
            }
        },
    }
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
        fetch_profile.model_request_headers = extensions.headers;
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
    let changed = &next != config;
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
    let has_provider_scoped_api_key = config_api_key.is_some();
    let official_endpoint = base_url.is_empty() || is_official_base_url(&base_url);
    // A provider-scoped token describes the active route and must win over a
    // long-lived auth.json login retained alongside it.
    let api_key = config_api_key
        .or_else(|| auth_api_key.map(ToString::to_string))
        .unwrap_or_default();
    let official = official_endpoint
        && !has_provider_scoped_api_key
        && (auth_mode == Some("chatgpt") || api_key.is_empty());
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
    document
        .get("model_provider")
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
    auth.get("auth_mode").and_then(Value::as_str) == Some("chatgpt")
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

fn resolve_native_status_executable(configured_codex_app_path: &str) -> PathBuf {
    resolve_codex_app_dir_with_saved(None, Some(configured_codex_app_path))
        .as_deref()
        .and_then(codex_runtime_executable)
        .unwrap_or_else(|| PathBuf::from("codex"))
}

fn native_login_status(codex_home: &Path, executable: Option<&Path>) -> NativeLoginStatus {
    const LOGIN_STATUS_TIMEOUT: Duration = Duration::from_secs(3);
    let executable = executable.unwrap_or_else(|| Path::new("codex"));
    let mut command = Command::new(executable);
    command
        .args(["login", "status"])
        .env("CODEX_HOME", codex_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(codey_runtime_core::windows_create_no_window());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return NativeLoginStatus::Unknown(format!(
                "无法运行 codex login status：{error}；executable={}；CODEX_HOME={}",
                executable.display(),
                codex_home.display()
            ));
        }
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < LOGIN_STATUS_TIMEOUT => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return NativeLoginStatus::Unknown(format!(
                    "codex login status 在 {}ms 后超时；executable={}；CODEX_HOME={}",
                    started.elapsed().as_millis(),
                    executable.display(),
                    codex_home.display()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return NativeLoginStatus::Unknown(format!(
                    "等待 codex login status 失败：{error}；executable={}；CODEX_HOME={}；elapsedMs={}",
                    executable.display(),
                    codex_home.display(),
                    started.elapsed().as_millis()
                ));
            }
        }
    }

    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => {
            return NativeLoginStatus::Unknown(format!(
                "读取 codex login status 输出失败：{error}；executable={}；CODEX_HOME={}；elapsedMs={}",
                executable.display(),
                codex_home.display(),
                started.elapsed().as_millis()
            ));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status = output.status.to_string();
    let diagnostic = format!(
        "executable={}；CODEX_HOME={}；exitStatus={status}；elapsedMs={}；stdoutBytes={}；stderrBytes={}",
        executable.display(),
        codex_home.display(),
        started.elapsed().as_millis(),
        output.stdout.len(),
        output.stderr.len()
    );
    match parse_native_login_status_output(output.status.success(), &stdout, &stderr, &status) {
        NativeLoginStatus::ChatGpt => NativeLoginStatus::ChatGpt,
        NativeLoginStatus::NotLoggedIn(reason) => {
            NativeLoginStatus::NotLoggedIn(format!("{reason}；{diagnostic}"))
        }
        NativeLoginStatus::ApiKey(reason) => {
            NativeLoginStatus::ApiKey(format!("{reason}；{diagnostic}"))
        }
        NativeLoginStatus::Unknown(reason) => {
            NativeLoginStatus::Unknown(format!("{reason}；{diagnostic}"))
        }
    }
}

fn native_login_status_with_path_fallback(
    codex_home: &Path,
    primary_executable: &Path,
) -> NativeLoginStatus {
    let primary_status = native_login_status(codex_home, Some(primary_executable));
    native_login_status_with_path_fallback_result(primary_executable, primary_status, || {
        native_login_status(codex_home, Some(Path::new("codex")))
    })
}

fn native_login_status_with_path_fallback_result(
    primary_executable: &Path,
    primary_status: NativeLoginStatus,
    fallback_probe: impl FnOnce() -> NativeLoginStatus,
) -> NativeLoginStatus {
    if !should_try_path_login_status_fallback(&primary_status, primary_executable) {
        return primary_status;
    }
    let primary_reason = match primary_status {
        NativeLoginStatus::Unknown(reason) => reason,
        status => return status,
    };
    match fallback_probe() {
        NativeLoginStatus::Unknown(fallback_reason) => NativeLoginStatus::Unknown(format!(
            "{primary_reason}；PATH codex 回退也无法确认官方登录状态：{fallback_reason}"
        )),
        status => status,
    }
}

fn should_try_path_login_status_fallback(status: &NativeLoginStatus, executable: &Path) -> bool {
    let NativeLoginStatus::Unknown(reason) = status else {
        return false;
    };
    if !(reason.contains("无法运行 codex login status")
        || reason.contains("could not run codex login status"))
    {
        return false;
    }
    executable != Path::new("codex")
}

fn parse_native_login_status_output(
    success: bool,
    stdout: &str,
    stderr: &str,
    status: &str,
) -> NativeLoginStatus {
    let normalized = format!("{stdout}\n{stderr}").trim().to_ascii_lowercase();
    if normalized.contains("not logged in")
        || normalized.contains("not signed in")
        || normalized.contains("authentication required")
        || normalized.contains("未登录")
    {
        NativeLoginStatus::NotLoggedIn(
            "Codex 原生认证探针明确返回未登录（未记录命令原始输出，避免泄露凭据）".to_string(),
        )
    } else if normalized.contains("api key") {
        NativeLoginStatus::ApiKey(
            "Codex 原生认证探针显示当前使用 API Key，而不是 ChatGPT 官方账号登录（未记录命令原始输出，避免泄露凭据）"
                .to_string(),
        )
    } else if success
        && (normalized.contains("chatgpt")
            || normalized.contains("oauth")
            || normalized.contains("bearer token"))
    {
        NativeLoginStatus::ChatGpt
    } else if success {
        NativeLoginStatus::Unknown("codex login status 输出格式未知".to_string())
    } else {
        NativeLoginStatus::Unknown(format!("codex login status 退出码为 {status}"))
    }
}

fn provider_config_api_key(
    document: &DocumentMut,
    provider: Option<&dyn TableLike>,
) -> Option<String> {
    provider_config_api_key_with_env(document, provider, &|name| std::env::var(name).ok())
}

fn provider_config_api_key_with_env(
    document: &DocumentMut,
    provider: Option<&dyn TableLike>,
    env_value: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
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

fn is_official_base_url(base_url: &str) -> bool {
    let base_url = base_url.to_ascii_lowercase();
    base_url.contains("chatgpt.com/backend-api/codex") || base_url.contains("api.openai.com")
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

    fn status_with_unknown_native(home: &Path) -> Result<OfficialAccountProfileStatus> {
        current_official_account_profile_status_with_probe(home, |_| {
            NativeLoginStatus::Unknown("probe unavailable".into())
        })
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
    fn official_capability_requires_chatgpt_tokens() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), "");
        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "access_token": "token" }
            }),
        );
        let OfficialAccountProfileStatus::Available(profile) =
            status_with_unknown_native(home.path()).unwrap()
        else {
            panic!("auth.json ChatGPT tokens should make official auth available");
        };
        assert!(profile.official_account);
        assert!(profile.supports_websockets);
        assert!(profile.supports_native_web_search);
        assert_eq!(profile.provider_id(), "openai");
        assert!(profile.api_key.is_empty());

        fs::remove_file(home.path().join("auth.json")).unwrap();
        write_config(home.path(), r#"cli_auth_credentials_store = "file""#);
        let OfficialAccountProfileStatus::Unavailable { reason } =
            status_with_unknown_native(home.path()).unwrap()
        else {
            panic!("missing file credentials under file store should be unavailable");
        };
        assert!(reason.contains("凭据存储策略为 file"));
        assert!(reason.contains("auth.json 不存在"));
    }

    #[test]
    fn official_auth_probe_distinguishes_file_missing_from_unknown_store() {
        let file_home = TempDir::new().unwrap();
        write_config(file_home.path(), r#"cli_auth_credentials_store = "file""#);
        let OfficialAccountProfileStatus::Unavailable { reason } =
            current_official_account_profile_status_with_probe(file_home.path(), |_| {
                NativeLoginStatus::Unknown("probe unavailable".into())
            })
            .unwrap()
        else {
            panic!("missing auth.json under file store should be unavailable");
        };
        assert!(reason.contains("probe unavailable"));
        assert!(reason.contains("凭据存储策略为 file"));

        let auto_home = TempDir::new().unwrap();
        write_config(auto_home.path(), r#"cli_auth_credentials_store = "auto""#);
        let status = current_official_account_profile_status_with_probe(auto_home.path(), |_| {
            NativeLoginStatus::Unknown("probe unavailable".into())
        })
        .unwrap();
        let OfficialAccountProfileStatus::Unknown { profile, reason } = status else {
            panic!("missing auth.json under auto store should be unknown");
        };
        assert!(profile.official_account);
        assert!(profile.supports_websockets);
        assert!(profile.supports_native_web_search);
        assert_eq!(profile.provider_id(), "openai");
        assert!(reason.contains("auth.json"));
    }

    #[test]
    fn native_login_status_wins_over_file_probe() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), r#"cli_auth_credentials_store = "keyring""#);

        let status = current_official_account_profile_status_with_probe(home.path(), |_| {
            NativeLoginStatus::ChatGpt
        })
        .unwrap();
        let OfficialAccountProfileStatus::Available(profile) = status else {
            panic!("native ChatGPT login should be authoritative");
        };
        assert!(profile.official_account);

        write_auth(
            home.path(),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "refresh_token": "stale-token" }
            }),
        );
        let OfficialAccountProfileStatus::Unavailable { reason } =
            current_official_account_profile_status_with_probe(home.path(), |_| {
                NativeLoginStatus::NotLoggedIn("native probe says not logged in".into())
            })
            .unwrap()
        else {
            panic!("native not-logged-in result should be authoritative");
        };
        assert!(reason.contains("native probe says not logged in"));
        assert!(reason.contains("chatgptTokenFields=[\"refresh_token\"]"));
    }

    #[test]
    fn native_login_status_parser_distinguishes_chatgpt_from_api_key() {
        assert_eq!(
            parse_native_login_status_output(true, "Logged in using ChatGPT", "", "exit status: 0",),
            NativeLoginStatus::ChatGpt
        );
        assert!(matches!(
            parse_native_login_status_output(
                true,
                "Logged in using an API key - sk-...",
                "",
                "exit status: 0",
            ),
            NativeLoginStatus::ApiKey(reason)
                if reason.contains("API Key") && !reason.contains("sk-")
        ));
        assert!(matches!(
            parse_native_login_status_output(false, "Not logged in", "", "exit status: 1"),
            NativeLoginStatus::NotLoggedIn(reason)
                if reason.contains("明确返回未登录")
        ));
        assert!(matches!(
            parse_native_login_status_output(true, "Unexpected auth mode", "", "exit status: 0"),
            NativeLoginStatus::Unknown(_)
        ));
    }

    #[test]
    fn native_login_status_tries_path_fallback_after_spawn_access_denied() {
        let status = native_login_status_with_path_fallback_result(
            Path::new(
                r"C:\Program Files\WindowsApps\OpenAI.Codex_26.820.7780.0_x64__2p2nqsd0c76g0\app\resources\codex.exe",
            ),
            NativeLoginStatus::Unknown(
                "无法运行 codex login status：拒绝访问。 (os error 5)".into(),
            ),
            || NativeLoginStatus::ChatGpt,
        );

        assert_eq!(status, NativeLoginStatus::ChatGpt);

        let status = native_login_status_with_path_fallback_result(
            Path::new("codex"),
            NativeLoginStatus::Unknown("无法运行 codex login status：not found".into()),
            || panic!("PATH fallback must not retry an identical codex executable"),
        );
        assert!(matches!(status, NativeLoginStatus::Unknown(_)));
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
    fn unknown_native_probe_keeps_keyring_and_auto_inconclusive() {
        for store in ["keyring", "auto"] {
            let home = TempDir::new().unwrap();
            write_config(
                home.path(),
                &format!(r#"cli_auth_credentials_store = "{store}""#),
            );
            write_auth(home.path(), serde_json::json!({}));

            let status = current_official_account_profile_status_with_probe(home.path(), |_| {
                NativeLoginStatus::Unknown("probe unavailable".into())
            })
            .unwrap();

            assert!(matches!(
                status,
                OfficialAccountProfileStatus::Unknown { .. }
            ));
        }
    }

    #[test]
    fn malformed_auth_json_is_unknown_for_launch_but_strict_provider_reads_still_fail() {
        let home = TempDir::new().unwrap();
        write_config(home.path(), "");
        fs::write(home.path().join("auth.json"), b"{").unwrap();

        let status = status_with_unknown_native(home.path()).unwrap();
        assert!(matches!(
            status,
            OfficialAccountProfileStatus::Unknown { .. }
        ));
        assert!(current_provider(home.path()).is_err());
    }

    #[test]
    fn official_capability_survives_ccswitch_selected_provider() {
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
            status_with_unknown_native(home.path()).unwrap()
        else {
            panic!("retained ChatGPT tokens should make official auth available");
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

        let OfficialAccountProfileStatus::Available(official) =
            status_with_unknown_native(home.path()).unwrap()
        else {
            panic!("retained ChatGPT login should remain available");
        };
        assert_eq!(official.provider_id(), "openai");
        assert!(official.official_account);

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
