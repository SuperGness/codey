//! Multiple ChatGPT (Codex official) accounts managed by Codey.
//!
//! Accounts are added through the same OAuth PKCE flow Codex and CLIProxyAPI
//! use and stored as complete `auth.json` documents under Codey's own config
//! directory. Exactly one account can be the default; making an account the
//! default copies its credentials into the Codex home so Codex itself (and the
//! local router, which reads the same file) run as that account.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const ACCOUNTS_DIR_NAME: &str = "official-accounts";
const DEFAULT_FILE_NAME: &str = "default.json";
const CODEX_AUTH_FILE_NAME: &str = "auth.json";

pub(crate) const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OAUTH_CALLBACK_PORT: u16 = 1455;
const OAUTH_SCOPE: &str = "openid profile email offline_access";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_CALLBACK_REQUEST_BYTES: usize = 16 * 1024;
const MAX_JWT_PAYLOAD_BYTES: usize = 64 * 1024;
/// Stored credentials older than this are refreshed before being handed to
/// Codex, so switching back to an account that idled for days still works.
const REFRESH_BEFORE_ACTIVATE_AGE: Duration = Duration::from_secs(6 * 60 * 60);

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialAccountRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub added_at: u64,
    /// Complete Codex `auth.json` document for this account.
    pub auth: Value,
}

/// Renderer-facing view of an account. Never carries tokens.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialAccountSummary {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub added_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<String>,
    pub is_default: bool,
}

impl OfficialAccountRecord {
    pub fn from_auth(auth: Value, added_at: u64) -> Result<Self> {
        let tokens = auth
            .get("tokens")
            .and_then(Value::as_object)
            .context("登录信息缺少 tokens 字段")?;
        let auth_mode_ok = matches!(auth.get("auth_mode"), None | Some(Value::Null))
            || auth.get("auth_mode").and_then(Value::as_str) == Some("chatgpt");
        if !auth_mode_ok {
            bail!("当前登录不是 ChatGPT 官方账号登录");
        }
        let access_token = tokens
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .context("登录信息缺少 access_token")?;
        let id_claims = tokens
            .get("id_token")
            .and_then(Value::as_str)
            .and_then(jwt_claims);
        let access_claims = jwt_claims(access_token);
        let account_id = tokens
            .get("account_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                [&id_claims, &access_claims]
                    .into_iter()
                    .flatten()
                    .find_map(chatgpt_account_id_from_claims)
            });
        let email = [&id_claims, &access_claims]
            .into_iter()
            .flatten()
            .find_map(|claims| string_claim(claims, "email"));
        let plan_type = [&id_claims, &access_claims]
            .into_iter()
            .flatten()
            .find_map(|claims| {
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|auth| string_claim(auth, "chatgpt_plan_type"))
            });
        let id = account_id
            .clone()
            .map(|account_id| sanitize_id(&account_id))
            .or_else(|| {
                email.as_deref().map(|email| {
                    format!(
                        "email-{}",
                        &sha256_hex(email.to_ascii_lowercase().as_bytes())[..24]
                    )
                })
            })
            .unwrap_or_else(|| format!("account-{}", uuid::Uuid::new_v4()));
        let mut auth = auth;
        if let Some(object) = auth.as_object_mut() {
            object.insert("auth_mode".to_string(), json!("chatgpt"));
            if let Some(account_id) = account_id.as_deref()
                && let Some(tokens) = object.get_mut("tokens").and_then(Value::as_object_mut)
                && !tokens.contains_key("account_id")
            {
                tokens.insert("account_id".to_string(), json!(account_id));
            }
        }
        Ok(Self {
            id,
            email,
            plan_type,
            account_id,
            added_at,
            auth,
        })
    }

    pub fn last_refresh(&self) -> Option<String> {
        self.auth
            .get("last_refresh")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    }

    fn last_refresh_age(&self, now: SystemTime) -> Option<Duration> {
        let raw = self.last_refresh()?;
        let refreshed = chrono::DateTime::parse_from_rfc3339(&raw).ok()?;
        let refreshed_unix = u64::try_from(refreshed.timestamp()).ok()?;
        let now_unix = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
        Some(Duration::from_secs(now_unix.saturating_sub(refreshed_unix)))
    }

    fn refresh_token(&self) -> Option<&str> {
        self.auth
            .get("tokens")
            .and_then(|tokens| tokens.get("refresh_token"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    pub fn summary(&self, default_id: Option<&str>) -> OfficialAccountSummary {
        OfficialAccountSummary {
            id: self.id.clone(),
            email: self.email.clone(),
            plan_type: self.plan_type.clone(),
            account_id: self.account_id.clone(),
            added_at: self.added_at,
            last_refresh: self.last_refresh(),
            is_default: default_id == Some(self.id.as_str()),
        }
    }

    /// Whether a Codex `auth.json` document belongs to this account.
    fn matches_auth(&self, auth: &Value) -> bool {
        match (&self.account_id, auth_account_id(auth)) {
            (Some(mine), Some(theirs)) => *mine == theirs,
            _ => {
                let mine = self
                    .auth
                    .get("tokens")
                    .and_then(|tokens| tokens.get("access_token"));
                let theirs = auth
                    .get("tokens")
                    .and_then(|tokens| tokens.get("access_token"));
                mine.is_some() && mine == theirs
            }
        }
    }
}

fn sanitize_id(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    if cleaned.is_empty() {
        format!("account-{}", uuid::Uuid::new_v4())
    } else {
        cleaned
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn string_claim(claims: &Value, key: &str) -> Option<String> {
    claims
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn chatgpt_account_id_from_claims(claims: &Value) -> Option<String> {
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| string_claim(auth, "chatgpt_account_id"))
        .or_else(|| string_claim(claims, "chatgpt_account_id"))
}

pub(crate) fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    if payload.len() > MAX_JWT_PAYLOAD_BYTES {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn auth_account_id(auth: &Value) -> Option<String> {
    let tokens = auth.get("tokens")?;
    tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            ["id_token", "access_token"].iter().find_map(|key| {
                tokens
                    .get(*key)
                    .and_then(Value::as_str)
                    .and_then(jwt_claims)
                    .as_ref()
                    .and_then(chatgpt_account_id_from_claims)
            })
        })
}

pub(crate) fn auth_is_chatgpt_login(auth: &Value) -> bool {
    (matches!(auth.get("auth_mode"), None | Some(Value::Null))
        || auth.get("auth_mode").and_then(Value::as_str) == Some("chatgpt"))
        && auth
            .get("tokens")
            .and_then(|tokens| tokens.get("access_token"))
            .and_then(Value::as_str)
            .is_some_and(|token| !token.trim().is_empty())
}

pub(crate) fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OfficialAccountStore {
    dir: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DefaultAccountFile {
    #[serde(default)]
    default_account_id: Option<String>,
}

/// Result of reconciling the default account with the Codex home at launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchLoginResolution {
    /// A default account exists and its credentials are in the Codex home.
    Available { account_id: String },
    /// No default account; the official route cannot be used.
    Unavailable { reason: String },
}

impl OfficialAccountStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn for_config_path(config_path: &Path) -> Self {
        let parent = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(parent.join(ACCOUNTS_DIR_NAME))
    }

    fn account_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", sanitize_id(id)))
    }

    fn default_path(&self) -> PathBuf {
        self.dir.join(DEFAULT_FILE_NAME)
    }

    pub fn list(&self) -> Result<Vec<OfficialAccountRecord>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("读取官方账号目录失败：{}", self.dir.display()));
            }
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json")
                || path.file_name().and_then(|name| name.to_str()) == Some(DEFAULT_FILE_NAME)
            {
                continue;
            }
            let bytes = fs::read(&path)
                .with_context(|| format!("读取官方账号文件失败：{}", path.display()))?;
            match serde_json::from_slice::<OfficialAccountRecord>(&bytes) {
                Ok(record) => records.push(record),
                Err(error) => {
                    crate::error_log::record_failure(
                        "official_account_file_invalid",
                        "official_accounts.list",
                        format!("{error}"),
                        json!({ "path": path.display().to_string() }),
                    );
                }
            }
        }
        records.sort_by(|a, b| a.added_at.cmp(&b.added_at).then_with(|| a.id.cmp(&b.id)));
        Ok(records)
    }

    pub fn get(&self, id: &str) -> Result<Option<OfficialAccountRecord>> {
        Ok(self.list()?.into_iter().find(|record| record.id == id))
    }

    pub fn default_account_id(&self) -> Result<Option<String>> {
        let bytes = match fs::read(self.default_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("读取默认官方账号记录失败"),
        };
        let file: DefaultAccountFile =
            serde_json::from_slice(&bytes).context("默认官方账号记录格式无效")?;
        Ok(file.default_account_id.filter(|id| !id.trim().is_empty()))
    }

    pub fn default_account(&self) -> Result<Option<OfficialAccountRecord>> {
        let Some(id) = self.default_account_id()? else {
            return Ok(None);
        };
        self.get(&id)
    }

    pub fn set_default_account_id(&self, id: Option<&str>) -> Result<()> {
        let file = DefaultAccountFile {
            default_account_id: id.map(ToString::to_string),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        crate::fs_util::atomic_write_private_with_parent(&self.default_path(), &bytes)
            .context("保存默认官方账号记录失败")
    }

    pub fn upsert(&self, record: &OfficialAccountRecord) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(record)?;
        crate::fs_util::atomic_write_private_with_parent(&self.account_path(&record.id), &bytes)
            .with_context(|| format!("保存官方账号失败：{}", record.id))
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        crate::fs_util::remove_file_if_exists(&self.account_path(id))
            .with_context(|| format!("删除官方账号失败：{id}"))?;
        if self.default_account_id()?.as_deref() == Some(id) {
            self.set_default_account_id(None)?;
        }
        Ok(())
    }

    pub fn summaries(&self) -> Result<Vec<OfficialAccountSummary>> {
        let default_id = self.default_account_id()?;
        Ok(self
            .list()?
            .into_iter()
            .map(|record| record.summary(default_id.as_deref()))
            .collect())
    }

    /// Reads the Codex home `auth.json` as an account record when it is a
    /// ChatGPT login.
    pub fn read_codex_login(codex_home: &Path) -> Result<Option<OfficialAccountRecord>> {
        let path = codex_home.join(CODEX_AUTH_FILE_NAME);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("读取 Codex 登录信息失败：{}", path.display()));
            }
        };
        let auth: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("Codex 登录信息格式无效：{}", path.display()))?;
        if !auth_is_chatgpt_login(&auth) {
            return Ok(None);
        }
        OfficialAccountRecord::from_auth(auth, unix_timestamp()).map(Some)
    }

    /// Copies an account's credentials into the Codex home.
    pub fn write_codex_login(codex_home: &Path, record: &OfficialAccountRecord) -> Result<()> {
        let path = codex_home.join(CODEX_AUTH_FILE_NAME);
        let bytes = serde_json::to_vec_pretty(&record.auth)?;
        if fs::read(&path).is_ok_and(|current| current == bytes) {
            return Ok(());
        }
        crate::fs_util::atomic_write_private_with_parent(&path, &bytes)
            .with_context(|| format!("写入 Codex 登录信息失败：{}", path.display()))
    }

    /// Codex refreshes tokens in place; pull a newer copy of the default
    /// account back into the store so switching away and back keeps working.
    pub fn sync_default_from_codex_home(&self, codex_home: &Path) -> Result<()> {
        let Some(mut record) = self.default_account()? else {
            return Ok(());
        };
        let path = codex_home.join(CODEX_AUTH_FILE_NAME);
        let Ok(bytes) = fs::read(&path) else {
            return Ok(());
        };
        let Ok(auth) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(());
        };
        if !auth_is_chatgpt_login(&auth) || !record.matches_auth(&auth) || auth == record.auth {
            return Ok(());
        }
        let newer = match (
            auth.get("last_refresh").and_then(Value::as_str),
            record.last_refresh(),
        ) {
            (Some(theirs), Some(mine)) => theirs >= mine.as_str(),
            (Some(_), None) => true,
            _ => false,
        };
        if !newer {
            return Ok(());
        }
        record.auth = auth;
        self.upsert(&record)
    }

    /// Ensures the Codex home reflects the default account. With no accounts
    /// stored, an existing ChatGPT login in the Codex home is adopted as the
    /// first (default) account so upgrades keep working without a re-login.
    pub fn resolve_launch_login(&self, codex_home: &Path) -> Result<LaunchLoginResolution> {
        self.sync_default_from_codex_home(codex_home)?;
        if let Some(record) = self.default_account()? {
            Self::write_codex_login(codex_home, &record)?;
            return Ok(LaunchLoginResolution::Available {
                account_id: record.id,
            });
        }
        if self.list()?.is_empty()
            && let Some(record) = Self::read_codex_login(codex_home)?
        {
            self.upsert(&record)?;
            self.set_default_account_id(Some(&record.id))?;
            return Ok(LaunchLoginResolution::Available {
                account_id: record.id,
            });
        }
        Ok(LaunchLoginResolution::Unavailable {
            reason: "Codey 中没有设为默认的官方账号；请在线路设置中添加官方账号并设为默认"
                .to_string(),
        })
    }

    /// Removes the Codex home login when it belongs to the given account.
    pub fn clear_codex_login_if_matches(
        codex_home: &Path,
        record: &OfficialAccountRecord,
    ) -> Result<bool> {
        let path = codex_home.join(CODEX_AUTH_FILE_NAME);
        let Ok(bytes) = fs::read(&path) else {
            return Ok(false);
        };
        let Ok(auth) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(false);
        };
        if !record.matches_auth(&auth) {
            return Ok(false);
        }
        crate::fs_util::remove_file_if_exists(&path)
            .with_context(|| format!("移除 Codex 登录信息失败：{}", path.display()))?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

/// Refreshes the account's tokens when they are old enough to matter. Errors
/// are returned so callers can decide whether to fall back to the stored copy.
pub async fn refresh_if_stale(
    client: &reqwest::Client,
    record: &mut OfficialAccountRecord,
) -> Result<bool> {
    let age = record.last_refresh_age(SystemTime::now());
    if age.is_some_and(|age| age < REFRESH_BEFORE_ACTIVATE_AGE) {
        return Ok(false);
    }
    let Some(refresh_token) = record.refresh_token().map(ToString::to_string) else {
        return Ok(false);
    };
    let response = client
        .post(OAUTH_TOKEN_URL)
        .timeout(Duration::from_secs(20))
        .json(&json!({
            "client_id": OAUTH_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "scope": "openid profile email",
        }))
        .send()
        .await
        .context("刷新官方账号令牌请求失败")?;
    let status = response.status();
    let body =
        crate::http_response::read_bounded_body(response, 256 * 1024, "刷新令牌响应").await?;
    if !status.is_success() {
        bail!("刷新官方账号令牌失败：{status}");
    }
    let payload: Value = serde_json::from_slice(&body).context("刷新令牌响应格式无效")?;
    apply_token_response(record, &payload)?;
    Ok(true)
}

fn apply_token_response(record: &mut OfficialAccountRecord, payload: &Value) -> Result<()> {
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .context("令牌响应缺少 access_token")?;
    let object = record
        .auth
        .as_object_mut()
        .context("账号登录信息格式无效")?;
    let tokens = object
        .entry("tokens")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("账号 tokens 格式无效")?;
    tokens.insert("access_token".to_string(), json!(access_token));
    if let Some(id_token) = payload.get("id_token").and_then(Value::as_str) {
        tokens.insert("id_token".to_string(), json!(id_token));
    }
    if let Some(refresh_token) = payload.get("refresh_token").and_then(Value::as_str) {
        tokens.insert("refresh_token".to_string(), json!(refresh_token));
    }
    object.insert("last_refresh".to_string(), json!(rfc3339_now()));
    Ok(())
}

// ---------------------------------------------------------------------------
// OAuth login sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum LoginPhase {
    Waiting,
    Completed(OfficialAccountRecord),
    Failed(String),
}

#[derive(Debug)]
pub struct LoginSession {
    pub auth_url: String,
    created_at: Instant,
    phase: Arc<Mutex<LoginPhase>>,
    task: tokio::task::JoinHandle<()>,
}

impl LoginSession {
    pub fn phase(&self) -> LoginPhase {
        self.phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn expired(&self) -> bool {
        self.created_at.elapsed() > LOGIN_TIMEOUT
    }

    pub fn cancel(&self) {
        self.task.abort();
    }
}

#[derive(Debug, Default)]
pub struct LoginSessions {
    sessions: HashMap<String, LoginSession>,
}

impl LoginSessions {
    pub fn insert(&mut self, id: String, session: LoginSession) {
        self.sessions.insert(id, session);
    }

    pub fn get(&self, id: &str) -> Option<&LoginSession> {
        self.sessions.get(id)
    }

    pub fn remove(&mut self, id: &str) -> Option<LoginSession> {
        self.sessions.remove(id)
    }

    /// Aborts every pending login so a new one can claim the callback port.
    pub fn cancel_all(&mut self) {
        for (_, session) in self.sessions.drain() {
            session.cancel();
        }
    }

    pub fn remove_expired(&mut self) {
        let expired = self
            .sessions
            .iter()
            .filter(|(_, session)| session.expired())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            if let Some(session) = self.sessions.remove(&id) {
                session.cancel();
            }
        }
    }
}

struct PkcePair {
    verifier: String,
    challenge: String,
}

fn pkce_pair() -> PkcePair {
    let mut seed = Vec::with_capacity(48);
    for _ in 0..3 {
        seed.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    let verifier = URL_SAFE_NO_PAD.encode(&seed);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    PkcePair {
        verifier,
        challenge,
    }
}

fn build_authorize_url(state: &str, challenge: &str) -> String {
    let mut url = reqwest::Url::parse(OAUTH_AUTHORIZE_URL).expect("static authorize url");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", OAUTH_REDIRECT_URI)
        .append_pair("scope", OAUTH_SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", "codex_cli_rs");
    url.to_string()
}

/// Starts a login: binds the OAuth callback port, returns the URL to open.
pub async fn start_login(client: reqwest::Client) -> Result<LoginSession> {
    let listener = TcpListener::bind(("127.0.0.1", OAUTH_CALLBACK_PORT))
        .await
        .map_err(|error| {
            anyhow!(
                "无法监听登录回调端口 127.0.0.1:{OAUTH_CALLBACK_PORT}（{error}）；请关闭正在进行的 codex login 或占用该端口的程序后重试"
            )
        })?;
    let pkce = pkce_pair();
    let state = uuid::Uuid::new_v4().simple().to_string();
    let auth_url = build_authorize_url(&state, &pkce.challenge);
    let phase = Arc::new(Mutex::new(LoginPhase::Waiting));
    let task_phase = Arc::clone(&phase);
    let task = tokio::spawn(async move {
        let outcome = tokio::time::timeout(
            LOGIN_TIMEOUT,
            run_callback_server(listener, client, state, pkce.verifier),
        )
        .await;
        let next = match outcome {
            Ok(Ok(record)) => LoginPhase::Completed(record),
            Ok(Err(error)) => LoginPhase::Failed(format!("{error:#}")),
            Err(_) => LoginPhase::Failed("登录等待超时，请重新开始添加账号".to_string()),
        };
        *task_phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    });
    Ok(LoginSession {
        auth_url,
        created_at: Instant::now(),
        phase,
        task,
    })
}

async fn run_callback_server(
    listener: TcpListener,
    client: reqwest::Client,
    expected_state: String,
    verifier: String,
) -> Result<OfficialAccountRecord> {
    loop {
        let (mut socket, _) = listener.accept().await.context("接受登录回调连接失败")?;
        let request = match read_request_head(&mut socket).await {
            Ok(request) => request,
            Err(_) => continue,
        };
        let Some(target) = request_target(&request) else {
            let _ = write_response(&mut socket, 400, "Bad Request").await;
            continue;
        };
        let Some(query) = target.strip_prefix("/auth/callback") else {
            let _ = write_response(&mut socket, 404, "Not Found").await;
            continue;
        };
        let params = query_params(query.trim_start_matches('?'));
        if params.get("state").map(String::as_str) != Some(expected_state.as_str()) {
            let _ = write_response(
                &mut socket,
                400,
                "登录状态校验失败，请回到 Codey 重新开始。",
            )
            .await;
            continue;
        }
        if let Some(error) = params.get("error") {
            let description = params.get("error_description").cloned().unwrap_or_default();
            let _ = write_response(&mut socket, 200, "登录已取消，可关闭此页面。").await;
            bail!("OpenAI 登录被拒绝：{error} {description}");
        }
        let Some(code) = params.get("code").filter(|code| !code.is_empty()) else {
            let _ = write_response(&mut socket, 400, "缺少授权码，请回到 Codey 重新开始。").await;
            continue;
        };
        let exchanged = exchange_code(&client, code, &verifier).await;
        match exchanged {
            Ok(record) => {
                let _ = write_response(
                    &mut socket,
                    200,
                    "登录成功，账号已添加到 Codey，可关闭此页面。",
                )
                .await;
                return Ok(record);
            }
            Err(error) => {
                let _ =
                    write_response(&mut socket, 500, "换取令牌失败，请回到 Codey 查看错误。").await;
                return Err(error);
            }
        }
    }
}

async fn read_request_head(socket: &mut tokio::net::TcpStream) -> Result<String> {
    let mut buffer = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut chunk))
            .await
            .context("读取登录回调请求超时")??;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n")
            || buffer.len() > MAX_CALLBACK_REQUEST_BYTES
        {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn request_target(request: &str) -> Option<&str> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    if method != "GET" {
        return None;
    }
    parts.next()
}

fn query_params(query: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://localhost/?{query}"))
        .map(|url| {
            url.query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

async fn write_response(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    message: &str,
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let body = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>Codey</title></head>\
         <body style=\"font-family:-apple-system,system-ui,sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;color:#222\">\
         <p style=\"font-size:18px\">{}</p></body></html>",
        html_escape(message)
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    let _ = socket.shutdown().await;
    Ok(())
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<OfficialAccountRecord> {
    let response = client
        .post(OAUTH_TOKEN_URL)
        .timeout(Duration::from_secs(20))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", OAUTH_REDIRECT_URI),
            ("client_id", OAUTH_CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .context("向 OpenAI 换取登录令牌失败")?;
    let status = response.status();
    let body =
        crate::http_response::read_bounded_body(response, 256 * 1024, "换取令牌响应").await?;
    if !status.is_success() {
        bail!("OpenAI 令牌接口返回 {status}");
    }
    let payload: Value = serde_json::from_slice(&body).context("令牌响应格式无效")?;
    record_from_token_response(&payload)
}

fn record_from_token_response(payload: &Value) -> Result<OfficialAccountRecord> {
    let id_token = payload
        .get("id_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .context("令牌响应缺少 access_token")?;
    let refresh_token = payload
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let auth = json!({
        "OPENAI_API_KEY": Value::Null,
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
        },
        "last_refresh": rfc3339_now(),
    });
    OfficialAccountRecord::from_auth(auth, unix_timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn unsigned_jwt(payload: Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        format!("{header}.{body}.sig")
    }

    fn chatgpt_auth(account_id: &str, email: &str, refreshed: &str) -> Value {
        json!({
            "OPENAI_API_KEY": null,
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": unsigned_jwt(json!({
                    "email": email,
                    "https://api.openai.com/auth": {
                        "chatgpt_account_id": account_id,
                        "chatgpt_plan_type": "plus",
                    }
                })),
                "access_token": format!("access-{account_id}"),
                "refresh_token": format!("refresh-{account_id}"),
            },
            "last_refresh": refreshed,
        })
    }

    #[test]
    fn record_derives_identity_from_id_token() {
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        assert_eq!(record.id, "acct_1");
        assert_eq!(record.account_id.as_deref(), Some("acct_1"));
        assert_eq!(record.email.as_deref(), Some("a@example.com"));
        assert_eq!(record.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            record.auth["tokens"]["account_id"],
            json!("acct_1"),
            "account_id is persisted for Codex"
        );
        let summary = record.summary(Some("acct_1"));
        assert!(summary.is_default);
        assert!(
            serde_json::to_string(&summary)
                .unwrap()
                .contains("a@example.com")
        );
        assert!(!serde_json::to_string(&summary).unwrap().contains("access-"));
    }

    #[test]
    fn record_rejects_api_key_logins() {
        let error = OfficialAccountRecord::from_auth(
            json!({ "auth_mode": "apikey", "OPENAI_API_KEY": "sk", "tokens": null }),
            1,
        )
        .unwrap_err();
        assert!(error.to_string().contains("tokens"));
        let error = OfficialAccountRecord::from_auth(
            json!({ "auth_mode": "apikey", "tokens": { "access_token": "x" } }),
            1,
        )
        .unwrap_err();
        assert!(error.to_string().contains("ChatGPT"));
    }

    #[test]
    fn store_lists_upserts_and_tracks_default() {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.default_account_id().unwrap(), None);

        let first = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        let second = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_2", "b@example.com", "2026-01-01T00:00:00Z"),
            2,
        )
        .unwrap();
        store.upsert(&first).unwrap();
        store.upsert(&second).unwrap();
        store.set_default_account_id(Some("acct_2")).unwrap();
        let summaries = store.summaries().unwrap();
        assert_eq!(summaries.len(), 2);
        assert!(!summaries[0].is_default);
        assert!(summaries[1].is_default);

        store.remove("acct_2").unwrap();
        assert_eq!(store.default_account_id().unwrap(), None);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn launch_resolution_writes_default_into_codex_home() {
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&record).unwrap();
        store.set_default_account_id(Some("acct_1")).unwrap();
        fs::write(
            home.path().join("auth.json"),
            br#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk"}"#,
        )
        .unwrap();

        let resolution = store.resolve_launch_login(home.path()).unwrap();
        assert_eq!(
            resolution,
            LaunchLoginResolution::Available {
                account_id: "acct_1".into()
            }
        );
        let written: Value =
            serde_json::from_slice(&fs::read(home.path().join("auth.json")).unwrap()).unwrap();
        assert_eq!(written, record.auth);
    }

    #[test]
    fn launch_resolution_adopts_existing_codex_login_once() {
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        fs::write(
            home.path().join("auth.json"),
            serde_json::to_vec(&chatgpt_auth(
                "acct_9",
                "z@example.com",
                "2026-01-01T00:00:00Z",
            ))
            .unwrap(),
        )
        .unwrap();
        let resolution = store.resolve_launch_login(home.path()).unwrap();
        assert_eq!(
            resolution,
            LaunchLoginResolution::Available {
                account_id: "acct_9".into()
            }
        );
        assert_eq!(
            store.default_account_id().unwrap().as_deref(),
            Some("acct_9")
        );

        // Removing the account must not re-adopt while other accounts exist.
        let other = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_2", "b@example.com", "2026-01-01T00:00:00Z"),
            2,
        )
        .unwrap();
        store.upsert(&other).unwrap();
        store.remove("acct_9").unwrap();
        let resolution = store.resolve_launch_login(home.path()).unwrap();
        assert!(matches!(
            resolution,
            LaunchLoginResolution::Unavailable { .. }
        ));
    }

    #[test]
    fn launch_resolution_without_accounts_or_login_is_unavailable() {
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        fs::write(
            home.path().join("auth.json"),
            br#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk"}"#,
        )
        .unwrap();
        let resolution = store.resolve_launch_login(home.path()).unwrap();
        assert!(matches!(
            resolution,
            LaunchLoginResolution::Unavailable { .. }
        ));
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn codex_refreshed_tokens_flow_back_into_the_store() {
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&record).unwrap();
        store.set_default_account_id(Some("acct_1")).unwrap();
        let mut refreshed = chatgpt_auth("acct_1", "a@example.com", "2026-02-01T00:00:00Z");
        refreshed["tokens"]["access_token"] = json!("access-new");
        fs::write(
            home.path().join("auth.json"),
            serde_json::to_vec(&refreshed).unwrap(),
        )
        .unwrap();

        store.sync_default_from_codex_home(home.path()).unwrap();
        let stored = store.get("acct_1").unwrap().unwrap();
        assert_eq!(stored.auth["tokens"]["access_token"], json!("access-new"));

        // A different account in the Codex home is left alone.
        let foreign = chatgpt_auth("acct_7", "q@example.com", "2026-03-01T00:00:00Z");
        fs::write(
            home.path().join("auth.json"),
            serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        store.sync_default_from_codex_home(home.path()).unwrap();
        let stored = store.get("acct_1").unwrap().unwrap();
        assert_eq!(stored.auth["tokens"]["access_token"], json!("access-new"));
    }

    #[test]
    fn authorize_url_and_callback_parsing_follow_codex_conventions() {
        let pkce = pkce_pair();
        assert!(pkce.verifier.len() >= 43 && pkce.verifier.len() <= 128);
        let url = build_authorize_url("state123", &pkce.challenge);
        assert!(url.starts_with(OAUTH_AUTHORIZE_URL));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("state=state123"));

        let target = request_target(
            "GET /auth/callback?code=abc&state=state123 HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .unwrap();
        let params = query_params(target.strip_prefix("/auth/callback?").unwrap());
        assert_eq!(params.get("code").unwrap(), "abc");
        assert_eq!(params.get("state").unwrap(), "state123");
        assert!(request_target("POST /auth/callback HTTP/1.1").is_none());
    }

    #[test]
    fn token_response_becomes_a_chatgpt_auth_document() {
        let payload = json!({
            "id_token": unsigned_jwt(json!({
                "email": "c@example.com",
                "https://api.openai.com/auth": { "chatgpt_account_id": "acct_c", "chatgpt_plan_type": "pro" }
            })),
            "access_token": "access-c",
            "refresh_token": "refresh-c",
        });
        let record = record_from_token_response(&payload).unwrap();
        assert_eq!(record.id, "acct_c");
        assert_eq!(record.auth["auth_mode"], json!("chatgpt"));
        assert_eq!(record.auth["tokens"]["account_id"], json!("acct_c"));
        assert!(record.auth["last_refresh"].as_str().is_some());
        assert!(record.last_refresh_age(SystemTime::now()).unwrap() < Duration::from_secs(60));
    }

    #[test]
    fn refresh_response_updates_tokens_and_timestamp() {
        let mut record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        apply_token_response(
            &mut record,
            &json!({ "access_token": "access-2", "refresh_token": "refresh-2" }),
        )
        .unwrap();
        assert_eq!(record.auth["tokens"]["access_token"], json!("access-2"));
        assert_eq!(record.auth["tokens"]["refresh_token"], json!("refresh-2"));
        assert_ne!(record.auth["last_refresh"], json!("2026-01-01T00:00:00Z"));
    }
}
