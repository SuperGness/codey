//! Multiple ChatGPT (Codex official) accounts managed by Codey.
//!
//! Accounts are added through the same OAuth PKCE flow Codex and CLIProxyAPI
//! use, by importing the current Codex login, or by pasting a refresh token
//! or OAuth JSON. A refresh token is exchanged for a full credential document.
//! JSON that already contains an access token is stored without exchanging it.
//! A still-valid access token with no refresh time is marked as just imported,
//! so the following usage query does not rotate the source refresh token.
//! Credentials are stored as complete `auth.json`
//! documents under Codey's own config directory. Exactly one account can be
//! the default; making an account the default copies its credentials into the
//! Codex home so Codex itself (and the local router, which reads the same
//! file) run as that account.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::{default_official_route_name, default_official_route_short_name};

pub const ACCOUNTS_DIR_NAME: &str = "official-accounts";
const DEFAULT_FILE_NAME: &str = "default.json";
const CODEX_AUTH_FILE_NAME: &str = "auth.json";
static STORE_WRITE_LOCK: Mutex<()> = Mutex::new(());

struct StoreWriteGuard {
    _file: fs::File,
    _thread: MutexGuard<'static, ()>,
}

pub(crate) const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_CALLBACK_PORT: u16 = 1455;
const OAUTH_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OAUTH_SCOPE: &str = "openid profile email offline_access";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_CALLBACK_REQUEST_BYTES: usize = 16 * 1024;
const MAX_JWT_PAYLOAD_BYTES: usize = 64 * 1024;
/// Stored credentials older than this are refreshed before being handed to
/// Codex, so switching back to an account that idled for days still works.
const REFRESH_BEFORE_ACTIVATE_AGE: Duration = Duration::from_secs(6 * 60 * 60);
/// Access tokens that expire within this margin are refreshed before use, so a
/// request never starts on a token that is about to lapse.
const TOKEN_REFRESH_MARGIN_SECONDS: u64 = 5 * 60;
/// Same bound as the local router's proxied upstream clients: a handful of
/// account proxies keep their TLS pools warm; a rare flood of new addresses
/// just drops the older pools.
const MAX_OFFICIAL_PROXY_CLIENTS: usize = 8;
/// 手动粘贴的 Refresh Token 或 OAuth JSON 上限。正常登录文档远小于此值。
const MAX_MANUAL_CREDENTIAL_BYTES: usize = 64 * 1024;
const MIN_REFRESH_TOKEN_CHARS: usize = 20;

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
    /// Route overrides Codey keeps for this account. They shape the derived
    /// official route while the account is the default; `None` keeps the value
    /// derived from the Codex configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_short_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_proxy: Option<String>,
    /// Custom OpenAI gateway for this account. `None` keeps the official
    /// Codex endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 官方明确拒绝该账号凭据时写入的原因与检测时间。网络故障、超时和
    /// 服务端 5xx 不会写入，刷新成功后清空。老记录没有这两个字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_since: Option<u64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_short_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 卡片据此把失效账号标红，并隐藏切换到该账号的入口。
    pub invalid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
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
        let plan_type = plan_type_from_auth(&auth);
        let id = account_id
            .clone()
            .map(|account_id| sanitize_id(&account_id))
            .or_else(|| email.as_deref().map(email_account_id))
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
            // A document rebuilt from `auth.json` carries credentials only;
            // `OfficialAccountStore::upsert` keeps the saved route settings.
            route_name: None,
            route_short_name: None,
            upstream_proxy: None,
            base_url: None,
            invalid_reason: None,
            invalid_since: None,
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
            route_name: self.route_name.clone(),
            route_short_name: self.route_short_name.clone(),
            upstream_proxy: self.upstream_proxy.clone(),
            base_url: self.base_url.clone(),
            invalid: self.invalid(),
            invalid_reason: self.invalid_reason.clone(),
            is_default: default_id == Some(self.id.as_str()),
        }
    }

    /// 官方是否明确拒绝过该账号的凭据。额度查询和切换默认都会先看这个标记。
    pub fn invalid(&self) -> bool {
        self.invalid_reason.is_some()
    }

    pub fn invalid_reason(&self) -> Option<&str> {
        self.invalid_reason.as_deref()
    }

    /// 记录失效。已有原因时保留首次检测时间，只更新原因文本。
    pub fn mark_invalid(&mut self, reason: &str) {
        if self.invalid_reason.is_none() {
            self.invalid_since = Some(unix_timestamp());
        }
        self.invalid_reason = Some(reason.to_string());
    }

    pub fn clear_invalid(&mut self) {
        self.invalid_reason = None;
        self.invalid_since = None;
    }

    /// 记录官方给出的套餐类型。只有确实变化时才返回 true，调用方据此决定
    /// 是否落盘；空值表示这次没能解析出套餐，保留已有结果。
    pub fn set_plan_type(&mut self, plan_type: Option<&str>) -> bool {
        let Some(plan) = plan_type.map(str::trim).filter(|value| !value.is_empty()) else {
            return false;
        };
        if self
            .plan_type
            .as_deref()
            .is_some_and(|current| current.eq_ignore_ascii_case(plan))
        {
            return false;
        }
        self.plan_type = Some(plan.to_string());
        true
    }

    /// 本地保存的 access token 是否仍在使用期限内。默认账号的凭据由 Codex
    /// 维护，判断额度接口 401 是否可信时用它排除尚未刷新的过期令牌。
    pub fn has_live_access_token(&self) -> bool {
        access_token_expires_at(self)
            .is_some_and(|expires_at| expires_at > unix_timestamp() + TOKEN_REFRESH_MARGIN_SECONDS)
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

fn read_account_record(path: &Path, operation: &str) -> Result<Option<OfficialAccountRecord>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("读取官方账号文件失败：{}", path.display()));
        }
    };
    match serde_json::from_slice::<OfficialAccountRecord>(&bytes) {
        Ok(record) => Ok(Some(record)),
        Err(error) => {
            crate::error_log::record_failure(
                "official_account_file_invalid",
                operation,
                format!("{error}"),
                json!({ "path": path.display().to_string() }),
            );
            Ok(None)
        }
    }
}

fn email_account_id(email: &str) -> String {
    format!(
        "email-{}",
        &crate::fs_util::sha256_hex(email.to_ascii_lowercase().as_bytes())[..24]
    )
}

/// `from_auth` 在既没有账号 ID 也没有邮箱时生成的占位 ID。
fn is_generated_account_id(id: &str) -> bool {
    id.strip_prefix("account-")
        .is_some_and(|rest| uuid::Uuid::parse_str(rest).is_ok())
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

/// 当前登录信息里的套餐类型。JWT 只在登录和刷新令牌时重建，所以升级或降级
/// 之后必须用最新一次解析结果覆盖账号记录，卡片才不会停在旧套餐上。
pub(crate) fn plan_type_from_auth(auth: &Value) -> Option<String> {
    let tokens = auth.get("tokens")?;
    let id_claims = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(jwt_claims);
    let access_claims = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .and_then(jwt_claims);
    [&id_claims, &access_claims]
        .into_iter()
        .flatten()
        .find_map(|claims| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| string_claim(auth, "chatgpt_plan_type"))
        })
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

pub(crate) fn access_token_issued_at(token: &str) -> Option<u64> {
    jwt_claims(token)?.get("iat").and_then(Value::as_u64)
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

/// Route overrides are stored trimmed, and a blank value means "no override".
fn route_setting(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 去掉首尾空白后仍非空的设置值。
fn trimmed_setting(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 从「官方账号1」或「官1」这类默认名称里取出编号，自定义名称返回 `None`。
fn route_index(value: Option<&str>) -> Option<usize> {
    let value = trimmed_setting(value)?;
    let digits = value
        .strip_prefix(crate::config::OFFICIAL_ROUTE_NAME_PREFIX)
        .or_else(|| value.strip_prefix(crate::config::OFFICIAL_ROUTE_SHORT_NAME))?;
    if let Ok(index) = digits.parse::<usize>() {
        return (index > 0).then_some(index);
    }
    // 第 10 个账号起短名称改用字母编号，这里换回同一个编号。
    let mut letters = digits.chars();
    let letter = letters.next()?;
    if letters.next().is_some() {
        return None;
    }
    crate::config::OFFICIAL_ROUTE_SHORT_NAME_LETTERS
        .chars()
        .position(|candidate| candidate == letter)
        .map(|position| position + 10)
}

/// 当前可用的最小编号；需要补短名称时跳过短名称已被占用的编号。
fn smallest_free_official_index(
    used_indices: &BTreeSet<usize>,
    used_short_names: &BTreeSet<String>,
    needs_short_name: bool,
) -> usize {
    (1..)
        .find(|index| {
            !used_indices.contains(index)
                && (!needs_short_name
                    || !used_short_names.contains(&default_official_route_short_name(*index)))
        })
        .unwrap_or(1)
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
    fn lock_writes(&self) -> Result<StoreWriteGuard> {
        let thread = STORE_WRITE_LOCK
            .lock()
            .map_err(|_| anyhow!("官方账号写入锁已损坏"))?;
        fs::create_dir_all(&self.dir)?;
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(self.dir.join(".write.lock"))?;
        file.lock_exclusive()?;
        Ok(StoreWriteGuard {
            _file: file,
            _thread: thread,
        })
    }

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

    pub(crate) fn account_path(&self, id: &str) -> PathBuf {
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
            if let Some(record) = read_account_record(&path, "official_accounts.list")? {
                records.push(record);
            }
        }
        records.sort_by(|a, b| a.added_at.cmp(&b.added_at).then_with(|| a.id.cmp(&b.id)));
        Ok(records)
    }

    pub fn get(&self, id: &str) -> Result<Option<OfficialAccountRecord>> {
        Ok(
            read_account_record(&self.account_path(id), "official_accounts.get")?
                .filter(|record| record.id == id),
        )
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
        let _guard = self.lock_writes()?;
        self.set_default_account_id_unlocked(id)
    }

    fn set_default_account_id_unlocked(&self, id: Option<&str>) -> Result<()> {
        let file = DefaultAccountFile {
            default_account_id: id.map(ToString::to_string),
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        crate::fs_util::atomic_write_private_with_parent(&self.default_path(), &bytes)
            .context("保存默认官方账号记录失败")
    }

    /// Stores the credentials of an account. Route settings belong to Codey
    /// rather than to the credential document, so the saved ones survive a
    /// re-login or a token refresh, which both arrive without them.
    pub fn upsert(&self, record: &OfficialAccountRecord) -> Result<()> {
        let _guard = self.lock_writes()?;
        let mut record = record.clone();
        if let Some(saved) = self.get(&record.id)? {
            record.route_name = saved.route_name;
            record.route_short_name = saved.route_short_name;
            record.upstream_proxy = saved.upstream_proxy;
            record.base_url = saved.base_url;
        }
        self.write(&record)
    }

    pub fn update_credentials_if_current(
        &self,
        expected: &OfficialAccountRecord,
        updated: &OfficialAccountRecord,
    ) -> Result<Option<OfficialAccountRecord>> {
        anyhow::ensure!(expected.id == updated.id, "官方账号身份不一致");
        let _guard = self.lock_writes()?;
        let Some(mut current) = self.get(&expected.id)? else {
            return Ok(None);
        };
        if current.auth == expected.auth
            && current.invalid_reason == expected.invalid_reason
            && current.invalid_since == expected.invalid_since
            && current.added_at == expected.added_at
        {
            current.auth = updated.auth.clone();
            current.invalid_reason = updated.invalid_reason.clone();
            current.invalid_since = updated.invalid_since;
            // 只有本次更新确实带来新套餐才覆盖：刷新期间用户可能已经打开线路
            // 菜单读到更新的套餐，不能被手上的旧快照盖回去。
            if updated.plan_type != expected.plan_type {
                current.plan_type = updated.plan_type.clone();
            }
            self.write(&current)?;
        }
        Ok(Some(current))
    }

    /// 用官方额度接口给出的实时套餐覆盖记录。额度接口每次读取都带当前套餐，
    /// 是降级或升级之后最先变化的数据源；只改套餐字段，凭据、失效标记和
    /// 线路设置都不受影响。
    pub fn update_plan_type(&self, id: &str, plan_type: Option<&str>) -> Result<bool> {
        let _guard = self.lock_writes()?;
        let Some(mut record) = self.get(id)? else {
            return Ok(false);
        };
        if !record.set_plan_type(plan_type) {
            return Ok(false);
        }
        self.write(&record)?;
        Ok(true)
    }

    /// Replaces the route overrides of one account; `None` restores the value
    /// derived from the Codex configuration.
    pub fn update_route_settings(
        &self,
        id: &str,
        route_name: Option<String>,
        route_short_name: Option<String>,
        upstream_proxy: Option<String>,
        base_url: Option<String>,
    ) -> Result<()> {
        let _guard = self.lock_writes()?;
        let mut record = self
            .get(id)?
            .ok_or_else(|| anyhow!("找不到官方账号：{id}"))?;
        record.route_name = route_setting(route_name);
        record.route_short_name = route_setting(route_short_name);
        record.upstream_proxy = route_setting(upstream_proxy);
        record.base_url = route_setting(base_url);
        self.write(&record)
    }

    /// 只回写运行时派生后的短名称：官方与第三方线路共用短名称命名空间，
    /// 派生时可能被占用的名字挤开，账号记录跟随调整后才与线路列表一致。
    pub fn update_route_short_name(&self, id: &str, short_name: &str) -> Result<()> {
        let _guard = self.lock_writes()?;
        let mut record = self
            .get(id)?
            .ok_or_else(|| anyhow!("找不到官方账号：{id}"))?;
        record.route_short_name = route_setting(Some(short_name.to_string()));
        self.write(&record)
    }

    /// 给还没有线路设置的账号补上按添加顺序生成的默认名称，例如「官方账号1」
    /// 和「官1」。编号取当前未被占用的最小编号，所以移除账号后新增的账号不会
    /// 和已有名称重复；已经保存过设置的账号原样保留。
    pub fn ensure_generated_route_settings(&self) -> Result<()> {
        let _guard = self.lock_writes()?;
        let records = self.list()?;
        let missing = |value: Option<&str>| trimmed_setting(value).is_none();
        let mut used_indices = BTreeSet::new();
        let mut used_short_names = BTreeSet::new();
        for record in &records {
            for value in [&record.route_name, &record.route_short_name] {
                if let Some(index) = route_index(value.as_deref()) {
                    used_indices.insert(index);
                }
            }
            if let Some(short_name) = trimmed_setting(record.route_short_name.as_deref()) {
                used_short_names.insert(short_name.to_string());
            }
        }
        for record in &records {
            let needs_name = missing(record.route_name.as_deref());
            let needs_short_name = missing(record.route_short_name.as_deref());
            if !needs_name && !needs_short_name {
                continue;
            }
            // 只缺其中一项时沿用已保存另一半的编号，名称和短名称保持同号。
            let index = route_index(record.route_name.as_deref())
                .or_else(|| route_index(record.route_short_name.as_deref()))
                .filter(|index| {
                    !needs_short_name
                        || !used_short_names.contains(&default_official_route_short_name(*index))
                })
                .unwrap_or_else(|| {
                    smallest_free_official_index(&used_indices, &used_short_names, needs_short_name)
                });
            used_indices.insert(index);
            let mut updated = record.clone();
            if needs_name {
                updated.route_name = Some(default_official_route_name(index));
            }
            if needs_short_name {
                let short_name = default_official_route_short_name(index);
                used_short_names.insert(short_name.clone());
                updated.route_short_name = Some(short_name);
            }
            self.write(&updated)?;
        }
        Ok(())
    }

    fn write(&self, record: &OfficialAccountRecord) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(record)?;
        crate::fs_util::atomic_write_private_with_parent(&self.account_path(&record.id), &bytes)
            .with_context(|| format!("保存官方账号失败：{}", record.id))
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let _guard = self.lock_writes()?;
        crate::fs_util::remove_file_if_exists(&self.account_path(id))
            .with_context(|| format!("删除官方账号失败：{id}"))?;
        if self.default_account_id()?.as_deref() == Some(id) {
            self.set_default_account_id_unlocked(None)?;
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

    /// Credential document the runtime reads for one account. Codex refreshes
    /// the default account's copy in place, so it keeps using the Codex home
    /// `auth.json`; every other account reads the document Codey stored.
    pub fn credential_path(&self, codex_home: &Path, account_id: &str) -> PathBuf {
        let is_default = self
            .default_account_id()
            .ok()
            .flatten()
            .is_some_and(|id| id == account_id);
        if is_default {
            codex_home.join(CODEX_AUTH_FILE_NAME)
        } else {
            self.account_path(account_id)
        }
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
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("读取 Codex 登录信息失败：{}", path.display()));
            }
        };
        let auth: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("Codex 登录信息格式无效：{}", path.display()))?;
        if !auth_is_chatgpt_login(&auth) || !record.matches_auth(&auth) || auth == record.auth {
            return Ok(());
        }
        let parse_refresh = |auth: &Value| {
            auth.get("last_refresh")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        };
        // 缺少可比较的刷新时间时保留 Codex 当前凭据，避免重新登录后
        // 又被账号库中的旧令牌覆盖。带时区的时间按实际时刻比较。
        let newer = match (parse_refresh(&auth), parse_refresh(&record.auth)) {
            (Some(theirs), Some(mine)) => theirs >= mine,
            _ => true,
        };
        if !newer {
            return Ok(());
        }
        let expected = record.clone();
        let plan_type = plan_type_from_auth(&auth);
        record.auth = auth;
        // 另一个 Codex 进程内的重新登录可能已经换了套餐。
        record.set_plan_type(plan_type.as_deref());
        // Codex 自己刷新成功说明凭据仍然有效，之前的失效标记不再成立。
        record.clear_invalid();
        self.update_credentials_if_current(&expected, &record)
            .map(|_| ())
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

/// 官方令牌端点返回 invalid_grant：refresh token 已被撤销，账号必须重新
/// 登录。调用方据此把账号标记为失效；网络故障、超时、5xx 和限流都会走
/// 普通错误，不会误标账号。
#[derive(Debug)]
pub struct OfficialAccountInvalid {
    reason: String,
    detail: Option<String>,
}

impl OfficialAccountInvalid {
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// 官方返回的原始错误描述，只用于请求日志，不进入界面文案。
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl std::fmt::Display for OfficialAccountInvalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.detail.as_deref() {
            Some(detail) => write!(formatter, "{}：{detail}", self.reason),
            None => write!(formatter, "{}", self.reason),
        }
    }
}

impl std::error::Error for OfficialAccountInvalid {}

/// OAuth 标准错误响应里 `invalid_grant` 表示刷新令牌已被拒绝。其他 OAuth
/// 错误（例如 `invalid_request`）说明请求本身有问题，不代表账号失效。
fn official_account_invalid_from_body(body: &[u8]) -> Option<OfficialAccountInvalid> {
    let payload = serde_json::from_slice::<Value>(body).ok()?;
    if payload.get("error").and_then(Value::as_str) != Some("invalid_grant") {
        return None;
    }
    Some(OfficialAccountInvalid {
        reason: "官方已撤销该账号的登录凭据，需要重新添加账号".to_string(),
        detail: payload
            .get("error_description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(ToString::to_string),
    })
}

/// Expiry time of the stored access token, when the token carries one.
pub(crate) fn access_token_expires_at(record: &OfficialAccountRecord) -> Option<u64> {
    record
        .auth
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(Value::as_str)
        .and_then(jwt_claims)?
        .get("exp")
        .and_then(Value::as_u64)
}

fn needs_token_refresh(record: &OfficialAccountRecord) -> bool {
    if access_token_expires_at(record)
        .is_some_and(|expires_at| expires_at <= unix_timestamp() + TOKEN_REFRESH_MARGIN_SECONDS)
    {
        return true;
    }
    record
        .last_refresh_age(SystemTime::now())
        .is_none_or(|age| age >= REFRESH_BEFORE_ACTIVATE_AGE)
}

/// Refreshes the account's tokens when the stored access token is expired,
/// close to expiry, or old enough to matter. Errors are returned so callers can
/// decide whether to fall back to the stored copy. Production callers pass the
/// shared proxy-client cache so repeated refreshes reuse the TLS pool.
pub async fn refresh_if_stale_cached(
    client: &reqwest::Client,
    record: &mut OfficialAccountRecord,
    upstream_proxy: Option<&str>,
    proxied_clients: Option<&Mutex<HashMap<String, reqwest::Client>>>,
) -> Result<bool> {
    // 已确认失效的账号不再尝试刷新：官方每次都会拒绝，重复请求只会抬高
    // 风控概率；重新添加账号会写入新凭据并清除失效标记。
    if record.invalid() {
        return Ok(false);
    }
    if !needs_token_refresh(record) {
        return Ok(false);
    }
    let Some(refresh_token) = record.refresh_token().map(ToString::to_string) else {
        return Ok(false);
    };
    let owned_client = official_http_client(client, proxied_clients, upstream_proxy)?;
    let response = owned_client
        .post(OAUTH_TOKEN_URL)
        .timeout(Duration::from_secs(20))
        .json(&refresh_token_grant(&refresh_token))
        .send()
        .await
        .context("刷新官方账号令牌请求失败")?;
    let status = response.status();
    let body =
        crate::http_response::read_bounded_body(response, 256 * 1024, "刷新令牌响应").await?;
    if !status.is_success() {
        if let Some(invalid) = official_account_invalid_from_body(&body) {
            return Err(anyhow::Error::new(invalid));
        }
        bail!("刷新官方账号令牌失败：{status}");
    }
    let payload: Value = serde_json::from_slice(&body).context("刷新令牌响应格式无效")?;
    apply_token_response(record, &payload)?;
    // 刷新成功说明凭据重新可用，之前记录的失效状态随之清除。
    record.clear_invalid();
    Ok(true)
}

#[cfg(test)]
pub async fn refresh_if_stale(
    client: &reqwest::Client,
    record: &mut OfficialAccountRecord,
    upstream_proxy: Option<&str>,
) -> Result<bool> {
    refresh_if_stale_cached(client, record, upstream_proxy, None).await
}

fn official_http_client(
    default_client: &reqwest::Client,
    proxied_clients: Option<&Mutex<HashMap<String, reqwest::Client>>>,
    upstream_proxy: Option<&str>,
) -> Result<reqwest::Client> {
    let Some(proxy) = upstream_proxy
        .map(str::trim)
        .filter(|proxy| !proxy.is_empty())
    else {
        return Ok(default_client.clone());
    };
    if let Some(cache) = proxied_clients {
        let mut clients = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(proxy) {
            return Ok(client.clone());
        }
        let client = build_official_proxy_client(proxy)?;
        if clients.len() >= MAX_OFFICIAL_PROXY_CLIENTS {
            clients.clear();
        }
        clients.insert(proxy.to_string(), client.clone());
        return Ok(client);
    }
    build_official_proxy_client(proxy)
}

fn build_official_proxy_client(proxy: &str) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .proxy(reqwest::Proxy::all(proxy).context("官方账号上游代理无效")?)
        .build()?)
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
    // 刷新回来的令牌带着当前套餐，升级或降级都要跟着记录更新，否则卡片会
    // 一直显示添加账号时的旧套餐。
    let plan_type = plan_type_from_auth(&record.auth);
    record.set_plan_type(plan_type.as_deref());
    Ok(())
}

fn refresh_token_grant(refresh_token: &str) -> Value {
    json!({
        "client_id": OAUTH_CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "scope": "openid profile email",
    })
}

/// 手动添加官方账号。
///
/// 只含 Refresh Token 时向官方换取完整登录信息。已经带 access token 的
/// OAuth JSON 原样保存，避免换票时轮换来源 Refresh Token。
pub async fn official_account_from_manual_input(
    client: &reqwest::Client,
    input: &str,
) -> Result<OfficialAccountRecord> {
    match parse_manual_official_credential(input)? {
        ManualOfficialCredential::Record(record) => Ok(*record),
        ManualOfficialCredential::RefreshToken(token) => {
            exchange_refresh_token(client, &token).await
        }
    }
}

enum ManualOfficialCredential {
    Record(Box<OfficialAccountRecord>),
    RefreshToken(String),
}

fn parse_manual_official_credential(input: &str) -> Result<ManualOfficialCredential> {
    if input.len() > MAX_MANUAL_CREDENTIAL_BYTES {
        bail!("输入过长，请只粘贴 Refresh Token 或一份 OAuth JSON");
    }
    let trimmed = prepare_manual_input(input);
    if trimmed.is_empty() {
        bail!("请粘贴 Refresh Token 或 OAuth JSON");
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let value: Value =
            serde_json::from_str(trimmed).map_err(|_| anyhow!("OAuth JSON 格式无效"))?;
        return credential_from_oauth_json(&value);
    }
    if trimmed.starts_with('"') {
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(Value::String(token)) => credential_from_refresh_token(&token),
            _ => bail!("OAuth JSON 格式无效"),
        };
    }
    credential_from_refresh_token(trimmed)
}

fn prepare_manual_input(input: &str) -> &str {
    let trimmed = input.trim().trim_start_matches('\u{feff}');
    let Some(fenced) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let fenced = fenced.trim_start();
    let fenced = fenced
        .strip_prefix("json")
        .or_else(|| fenced.strip_prefix("JSON"))
        .unwrap_or(fenced)
        .trim();
    fenced.strip_suffix("```").unwrap_or(fenced).trim()
}

fn credential_from_refresh_token(raw: &str) -> Result<ManualOfficialCredential> {
    let token = normalize_bare_token(raw);
    if token.is_empty() {
        bail!("请粘贴 Refresh Token 或 OAuth JSON");
    }
    if token.chars().count() < MIN_REFRESH_TOKEN_CHARS {
        bail!("Refresh Token 无效");
    }
    if token.starts_with("sk-") {
        bail!("这是 API Key，官方账号需要 Refresh Token 或 OAuth JSON");
    }
    if looks_like_jwt(&token) {
        bail!("这是 Access Token，请粘贴 Refresh Token，或包含 refresh_token 的 OAuth JSON");
    }
    Ok(ManualOfficialCredential::RefreshToken(token))
}

fn normalize_bare_token(raw: &str) -> String {
    let mut text = raw.trim();
    if text.len() >= 6 && text[..6].eq_ignore_ascii_case("bearer") {
        let rest = &text[6..];
        if rest.chars().next().is_some_and(char::is_whitespace) {
            text = rest.trim();
        }
    }
    let compact: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let bytes = compact.as_bytes();
    if compact.len() >= 2
        && ((bytes[0] == b'"' && bytes[compact.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[compact.len() - 1] == b'\''))
    {
        return compact[1..compact.len() - 1].to_string();
    }
    compact
}

fn looks_like_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty()
    )
}

fn credential_from_oauth_json(value: &Value) -> Result<ManualOfficialCredential> {
    if !value.is_object() {
        bail!("OAuth JSON 必须是对象");
    }
    let source = oauth_json_source(value)?;
    let access_token = token_string(source, &["access_token", "accessToken"]);
    let refresh_token = token_string(source, &["refresh_token", "refreshToken", "rt"]);
    let id_token = token_string(source, &["id_token", "idToken"]);
    let account_id = token_string(source, &["account_id", "accountId"])
        .or_else(|| string_field(value, &["account_id", "accountId"]));
    let email = string_field(source, &["email"]).or_else(|| string_field(value, &["email"]));
    let plan = string_field(source, &["plan_type", "planType", "chatgpt_plan_type"])
        .or_else(|| string_field(value, &["plan_type", "planType", "chatgpt_plan_type"]));
    let Some(access_token) = access_token else {
        let Some(refresh_token) = refresh_token else {
            bail!("OAuth JSON 里没有 refresh_token 或 access_token");
        };
        return credential_from_refresh_token(&refresh_token);
    };
    if let Some(mode) = source.get("auth_mode").and_then(Value::as_str)
        && mode != "chatgpt"
    {
        bail!("当前登录不是 ChatGPT 官方账号登录");
    }
    let auth = auth_document_from_oauth_json(
        source,
        &access_token,
        refresh_token.as_deref(),
        id_token.as_deref(),
        account_id.as_deref(),
    );
    let mut record = OfficialAccountRecord::from_auth(auth, unix_timestamp())?;
    apply_declared_identity(&mut record, email, plan);
    // 仍在有效期内、又没有刷新时间的登录文档，按刚刚导入处理。
    // 否则接下来的额度查询会立刻轮换 Refresh Token，使来源登录失效。
    if record.last_refresh().is_none()
        && record.has_live_access_token()
        && let Some(object) = record.auth.as_object_mut()
    {
        object.insert("last_refresh".to_string(), json!(rfc3339_now()));
    }
    Ok(ManualOfficialCredential::Record(Box::new(record)))
}

fn oauth_json_source(value: &Value) -> Result<&Value> {
    if token_string(
        value,
        &[
            "access_token",
            "accessToken",
            "refresh_token",
            "refreshToken",
            "rt",
        ],
    )
    .is_some()
    {
        return Ok(value);
    }
    if let Some(auth) = value.get("auth")
        && token_string(
            auth,
            &[
                "access_token",
                "accessToken",
                "refresh_token",
                "refreshToken",
                "rt",
            ],
        )
        .is_some()
    {
        return Ok(auth);
    }
    bail!("OAuth JSON 里没有 refresh_token 或 access_token")
}

fn token_string(document: &Value, keys: &[&str]) -> Option<String> {
    document
        .get("tokens")
        .and_then(|tokens| string_field(tokens, keys))
        .or_else(|| string_field(document, keys))
}

fn string_field(document: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        document
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn auth_document_from_oauth_json(
    source: &Value,
    access_token: &str,
    refresh_token: Option<&str>,
    id_token: Option<&str>,
    account_id: Option<&str>,
) -> Value {
    if source.get("tokens").and_then(Value::as_object).is_some() {
        let mut auth = source.clone();
        if let Some(tokens) = auth.get_mut("tokens").and_then(Value::as_object_mut) {
            insert_token_if_blank(tokens, "access_token", Some(access_token));
            insert_token_if_blank(tokens, "refresh_token", refresh_token);
            insert_token_if_blank(tokens, "id_token", id_token);
            insert_token_if_blank(tokens, "account_id", account_id);
        }
        return auth;
    }
    let mut tokens = serde_json::Map::new();
    tokens.insert("access_token".to_string(), json!(access_token));
    tokens.insert(
        "refresh_token".to_string(),
        json!(refresh_token.unwrap_or("")),
    );
    tokens.insert("id_token".to_string(), json!(id_token.unwrap_or("")));
    if let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) {
        tokens.insert("account_id".to_string(), json!(account_id));
    }
    let mut auth = serde_json::Map::new();
    auth.insert("OPENAI_API_KEY".to_string(), Value::Null);
    auth.insert("auth_mode".to_string(), json!("chatgpt"));
    auth.insert("tokens".to_string(), Value::Object(tokens));
    if let Some(last_refresh) = source
        .get("last_refresh")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        auth.insert("last_refresh".to_string(), json!(last_refresh));
    }
    Value::Object(auth)
}

fn insert_token_if_blank(
    tokens: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let occupied = tokens
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|current| !current.trim().is_empty());
    if !occupied {
        tokens.insert(key.to_string(), json!(value));
    }
}

fn apply_declared_identity(
    record: &mut OfficialAccountRecord,
    email: Option<String>,
    plan: Option<String>,
) {
    if record.email.is_none() {
        record.email = email.filter(|value| !value.is_empty());
    }
    if record.plan_type.is_none() {
        record.plan_type = plan.filter(|value| !value.is_empty());
    }
    if record.account_id.is_none()
        && is_generated_account_id(&record.id)
        && let Some(email) = record.email.clone()
    {
        record.id = email_account_id(&email);
    }
}

async fn exchange_refresh_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<OfficialAccountRecord> {
    let response = client
        .post(OAUTH_TOKEN_URL)
        .timeout(Duration::from_secs(20))
        .json(&refresh_token_grant(refresh_token))
        .send()
        .await
        .context("用 Refresh Token 换取登录信息失败")?;
    let status = response.status();
    let body =
        crate::http_response::read_bounded_body(response, 256 * 1024, "换取登录信息响应").await?;
    if !status.is_success() {
        if official_account_invalid_from_body(&body).is_some() {
            bail!("Refresh Token 已被拒绝，请确认令牌仍然有效");
        }
        bail!("用 Refresh Token 换取登录信息失败：{status}");
    }
    let mut payload: Value = serde_json::from_slice(&body).context("登录信息响应格式无效")?;
    let missing_refresh = payload
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .is_none();
    if missing_refresh && let Some(object) = payload.as_object_mut() {
        object.insert("refresh_token".to_string(), json!(refresh_token));
    }
    record_from_token_response(&payload).context("Refresh Token 换到的登录信息不完整")
}

// ---------------------------------------------------------------------------
// OAuth login sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum LoginPhase {
    Waiting,
    /// Boxed so a waiting session, which is the common phase, stays small.
    Completed(Box<OfficialAccountRecord>),
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

async fn bind_callback_listener(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port)).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!("登录回调端口 127.0.0.1:{port} 已被占用，当前登录流程使用固定回调地址；请释放该端口后重试，或导入 Codex 现有登录")
        } else {
            anyhow!("无法监听登录回调端口 127.0.0.1:{port}（{error}）")
        }
    })
}

/// Starts a login: binds the OAuth callback port, returns the URL to open.
pub async fn start_login(client: reqwest::Client) -> Result<LoginSession> {
    let listener = bind_callback_listener(OAUTH_CALLBACK_PORT).await?;
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
            Ok(Ok(record)) => LoginPhase::Completed(Box::new(record)),
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

    #[test]
    fn credential_commits_preserve_route_edits_and_do_not_recreate_removed_accounts() {
        let directory = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(directory.path());
        let original = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&original).unwrap();
        let mut refreshed = original.clone();
        apply_token_response(&mut refreshed, &json!({"access_token":"refreshed"})).unwrap();
        store
            .update_route_settings(
                &original.id,
                Some("new route".into()),
                None,
                Some("http://127.0.0.1:1234".into()),
                Some("https://gateway.example/backend-api/codex".into()),
            )
            .unwrap();
        let committed = store
            .update_credentials_if_current(&original, &refreshed)
            .unwrap()
            .unwrap();
        assert_eq!(committed.route_name.as_deref(), Some("new route"));
        assert_eq!(
            committed.upstream_proxy.as_deref(),
            Some("http://127.0.0.1:1234")
        );
        assert_eq!(
            committed.base_url.as_deref(),
            Some("https://gateway.example/backend-api/codex")
        );
        assert_eq!(committed.auth, refreshed.auth);
        store.remove(&original.id).unwrap();
        assert!(
            store
                .update_credentials_if_current(&original, &refreshed)
                .unwrap()
                .is_none()
        );
        assert!(store.get(&original.id).unwrap().is_none());
    }

    #[test]
    fn old_refresh_and_rejection_cannot_replace_new_login_credentials() {
        let directory = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(directory.path());
        let original = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&original).unwrap();
        let mut new_login = original.clone();
        apply_token_response(&mut new_login, &json!({"access_token":"new-login"})).unwrap();
        store.upsert(&new_login).unwrap();
        let mut stale = original.clone();
        stale.mark_invalid("expired");
        let actual = store
            .update_credentials_if_current(&original, &stale)
            .unwrap()
            .unwrap();
        assert_eq!(actual.auth, new_login.auth);
        assert!(!actual.invalid());
    }

    #[tokio::test]
    async fn invalid_account_does_not_attempt_token_refresh() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        record.mark_invalid("revoked");
        let original = record.auth.clone();
        let refreshed = refresh_if_stale(&client, &mut record, Some("invalid proxy URL"))
            .await
            .unwrap();
        assert!(!refreshed);
        assert!(record.invalid());
        assert_eq!(record.auth, original);
    }

    #[tokio::test]
    async fn token_refresh_uses_account_proxy_without_falling_back_to_direct() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 4096];
            let count = stream.read(&mut bytes).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            String::from_utf8_lossy(&bytes[..count]).to_string()
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        let original = record.auth.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            refresh_if_stale(&client, &mut record, Some(&format!("http://{address}"))),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        let request = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(request.starts_with("CONNECT auth.openai.com:443 "));
        assert_eq!(record.auth, original);
    }

    #[test]
    fn official_proxy_clients_are_reused_for_the_same_proxy() {
        let cache = Mutex::new(HashMap::new());
        let default_client = reqwest::Client::builder().no_proxy().build().unwrap();
        official_http_client(&default_client, Some(&cache), Some("http://127.0.0.1:9")).unwrap();
        official_http_client(&default_client, Some(&cache), Some("http://127.0.0.1:9")).unwrap();
        official_http_client(&default_client, Some(&cache), Some("http://127.0.0.1:10")).unwrap();
        assert!(
            official_http_client(&default_client, Some(&cache), Some("not a proxy url")).is_err()
        );
        let cached = cache.lock().unwrap();
        assert_eq!(cached.len(), 2);
        assert!(cached.contains_key("http://127.0.0.1:9"));
        assert!(cached.contains_key("http://127.0.0.1:10"));
    }

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
    fn get_reads_only_the_requested_account_file() {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&record).unwrap();
        fs::write(store.account_path("acct_2"), "{not-json").unwrap();
        fs::write(
            store.account_path("acct_3"),
            serde_json::to_vec(&json!({
                "id": "someone-else",
                "addedAt": 1,
                "auth": { "auth_mode": "chatgpt", "tokens": { "access_token": "x" } }
            }))
            .unwrap(),
        )
        .unwrap();

        let loaded = store.get("acct_1").unwrap().unwrap();
        assert_eq!(loaded.id, "acct_1");
        assert!(store.get("acct_2").unwrap().is_none());
        assert!(store.get("acct_3").unwrap().is_none());
        assert!(store.get("missing").unwrap().is_none());
    }

    #[test]
    fn route_settings_outlive_credential_updates_and_can_be_cleared() {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&record).unwrap();
        store
            .update_route_settings(
                "acct_1",
                Some("  主力官方号  ".into()),
                Some("主".into()),
                // A blank value means "no override", not an empty proxy.
                Some("   ".into()),
                Some("  https://gateway.example/v1/  ".into()),
            )
            .unwrap();
        let saved = store.get("acct_1").unwrap().unwrap();
        assert_eq!(saved.route_name.as_deref(), Some("主力官方号"));
        assert_eq!(saved.route_short_name.as_deref(), Some("主"));
        assert_eq!(saved.upstream_proxy, None);
        assert_eq!(
            saved.base_url.as_deref(),
            Some("https://gateway.example/v1/")
        );

        // Re-logging in rebuilds the record from `auth.json`, which carries
        // credentials only; the saved route must stay.
        let relogin = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-02-01T00:00:00Z"),
            2,
        )
        .unwrap();
        store.upsert(&relogin).unwrap();
        let summary = store
            .summaries()
            .unwrap()
            .into_iter()
            .find(|summary| summary.id == "acct_1")
            .unwrap();
        assert_eq!(summary.route_name.as_deref(), Some("主力官方号"));
        assert_eq!(summary.route_short_name.as_deref(), Some("主"));
        assert_eq!(
            summary.base_url.as_deref(),
            Some("https://gateway.example/v1/")
        );

        store
            .update_route_settings("acct_1", None, None, None, None)
            .unwrap();
        let cleared = store.get("acct_1").unwrap().unwrap();
        assert_eq!(cleared.route_name, None);
        assert_eq!(cleared.route_short_name, None);
        assert_eq!(cleared.base_url, None);

        let error = store
            .update_route_settings("acct_missing", None, None, None, None)
            .unwrap_err();
        assert!(error.to_string().contains("acct_missing"));
    }

    #[test]
    fn generated_route_settings_number_accounts_and_keep_custom_names() {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        for (index, id) in ["acct_1", "acct_2", "acct_3"].into_iter().enumerate() {
            let record = OfficialAccountRecord::from_auth(
                chatgpt_auth(id, &format!("{id}@example.com"), "2026-01-01T00:00:00Z"),
                index as u64 + 1,
            )
            .unwrap();
            store.upsert(&record).unwrap();
        }

        store.ensure_generated_route_settings().unwrap();
        let generated = store
            .list()
            .unwrap()
            .into_iter()
            .map(|record| {
                (
                    record.id,
                    record.route_name.unwrap_or_default(),
                    record.route_short_name.unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            generated,
            vec![
                (
                    "acct_1".to_string(),
                    "官方账号1".to_string(),
                    "官1".to_string()
                ),
                (
                    "acct_2".to_string(),
                    "官方账号2".to_string(),
                    "官2".to_string()
                ),
                (
                    "acct_3".to_string(),
                    "官方账号3".to_string(),
                    "官3".to_string()
                ),
            ]
        );

        // 重复执行不会改写已经生成的名称，手动设置的名称也不会被覆盖。
        store.ensure_generated_route_settings().unwrap();
        store
            .update_route_settings(
                "acct_2",
                Some("主力官方号".into()),
                Some("主".into()),
                None,
                None,
            )
            .unwrap();
        store.ensure_generated_route_settings().unwrap();
        let custom = store.get("acct_2").unwrap().unwrap();
        assert_eq!(custom.route_name.as_deref(), Some("主力官方号"));
        assert_eq!(custom.route_short_name.as_deref(), Some("主"));

        // 移除账号 1 后新增的账号取当前最小编号，不会和账号 3 的「官方账号3」重复。
        store.remove("acct_1").unwrap();
        let added_later = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_4", "acct_4@example.com", "2026-01-01T00:00:00Z"),
            4,
        )
        .unwrap();
        store.upsert(&added_later).unwrap();
        store.ensure_generated_route_settings().unwrap();
        let renumbered = store.get("acct_4").unwrap().unwrap();
        assert_eq!(renumbered.route_name.as_deref(), Some("官方账号1"));
        assert_eq!(renumbered.route_short_name.as_deref(), Some("官1"));
        let untouched = store.get("acct_3").unwrap().unwrap();
        assert_eq!(untouched.route_name.as_deref(), Some("官方账号3"));
        assert_eq!(untouched.route_short_name.as_deref(), Some("官3"));

        // 只缺一半设置时，补上的名称沿用另一半的编号。
        store
            .update_route_settings("acct_3", Some("官方账号7".into()), None, None, None)
            .unwrap();
        store.ensure_generated_route_settings().unwrap();
        let paired = store.get("acct_3").unwrap().unwrap();
        assert_eq!(paired.route_name.as_deref(), Some("官方账号7"));
        assert_eq!(paired.route_short_name.as_deref(), Some("官7"));
    }

    #[test]
    fn generated_short_names_keep_numbering_after_the_ninth_account() {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        for index in 1..=11u64 {
            let id = format!("acct_{index}");
            let record = OfficialAccountRecord::from_auth(
                chatgpt_auth(&id, &format!("{id}@example.com"), "2026-01-01T00:00:00Z"),
                index,
            )
            .unwrap();
            store.upsert(&record).unwrap();
        }

        store.ensure_generated_route_settings().unwrap();
        // 短名称只有两个字符，第 10 个账号起改用字母编号。
        assert_eq!(
            store
                .get("acct_10")
                .unwrap()
                .unwrap()
                .route_short_name
                .as_deref(),
            Some("官A")
        );
        assert_eq!(
            store
                .get("acct_11")
                .unwrap()
                .unwrap()
                .route_short_name
                .as_deref(),
            Some("官B")
        );

        // 字母编号对应同一个编号，重复补齐不会重新分配已用值。
        store.ensure_generated_route_settings().unwrap();
        let names = store
            .list()
            .unwrap()
            .into_iter()
            .filter_map(|record| record.route_short_name)
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), 11);
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
    fn launch_resolution_preserves_current_credentials_without_comparable_refresh_times() {
        for (saved_refresh, current_refresh) in [
            (Some("2026-01-01T00:00:00Z"), None),
            (Some("2026-01-01T00:00:00Z"), Some("invalid")),
            (None, None),
            (Some("invalid"), Some("2026-01-01T00:00:00Z")),
        ] {
            let dir = TempDir::new().unwrap();
            let home = TempDir::new().unwrap();
            let store = OfficialAccountStore::new(dir.path());
            let mut saved_auth = chatgpt_auth("acct_1", "a@example.com", "unused");
            saved_auth["last_refresh"] = json!(saved_refresh);
            let mut saved = OfficialAccountRecord::from_auth(saved_auth, 1).unwrap();
            saved.route_name = Some("saved route".into());
            saved.mark_invalid("old credentials rejected");
            store.upsert(&saved).unwrap();
            store.set_default_account_id(Some(&saved.id)).unwrap();
            let mut current = saved.auth.clone();
            current["tokens"]["access_token"] = json!("new-login-access");
            current["tokens"]["refresh_token"] = json!("new-login-refresh");
            if let Some(refresh) = current_refresh {
                current["last_refresh"] = json!(refresh);
            } else {
                current.as_object_mut().unwrap().remove("last_refresh");
            }
            fs::write(
                home.path().join("auth.json"),
                serde_json::to_vec(&current).unwrap(),
            )
            .unwrap();

            for _ in 0..2 {
                assert_eq!(
                    store.resolve_launch_login(home.path()).unwrap(),
                    LaunchLoginResolution::Available {
                        account_id: saved.id.clone()
                    }
                );
                let stored = store.default_account().unwrap().unwrap();
                assert_eq!(stored.auth, current);
                assert!(!stored.invalid());
                assert_eq!(stored.route_name, saved.route_name);
                assert_eq!(
                    OfficialAccountStore::read_codex_login(home.path())
                        .unwrap()
                        .unwrap()
                        .auth,
                    current
                );
            }
        }
    }

    #[test]
    fn launch_resolution_compares_refresh_times_as_instants() {
        for (current_refresh, use_current) in [
            ("2026-01-01T08:00:00+08:00", false),
            ("2025-12-31T23:00:00-03:00", true),
            ("2026-01-01T09:00:00+08:00", true),
        ] {
            let dir = TempDir::new().unwrap();
            let home = TempDir::new().unwrap();
            let store = OfficialAccountStore::new(dir.path());
            let saved = OfficialAccountRecord::from_auth(
                chatgpt_auth("acct_1", "a@example.com", "2026-01-01T01:00:00Z"),
                1,
            )
            .unwrap();
            store.upsert(&saved).unwrap();
            store.set_default_account_id(Some(&saved.id)).unwrap();
            let mut current = saved.auth.clone();
            current["last_refresh"] = json!(current_refresh);
            current["tokens"]["access_token"] = json!("current-access");
            fs::write(
                home.path().join("auth.json"),
                serde_json::to_vec(&current).unwrap(),
            )
            .unwrap();

            store.resolve_launch_login(home.path()).unwrap();
            let expected = if use_current { current } else { saved.auth };
            assert_eq!(store.default_account().unwrap().unwrap().auth, expected);
            assert_eq!(
                OfficialAccountStore::read_codex_login(home.path())
                    .unwrap()
                    .unwrap()
                    .auth,
                expected
            );
        }
    }

    #[test]
    fn launch_resolution_restores_missing_auth_but_preserves_unreadable_auth() {
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path());
        let saved = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&saved).unwrap();
        store.set_default_account_id(Some(&saved.id)).unwrap();
        store.resolve_launch_login(home.path()).unwrap();
        let path = home.path().join("auth.json");
        let restored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored, saved.auth);

        fs::write(&path, b"{unfinished").unwrap();
        let error = store.resolve_launch_login(home.path()).unwrap_err();
        assert!(error.to_string().contains("Codex 登录信息格式无效"));
        assert_eq!(fs::read(&path).unwrap(), b"{unfinished");

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let error = store.resolve_launch_login(home.path()).unwrap_err();
        assert!(error.to_string().contains("读取 Codex 登录信息失败"));
        assert!(path.is_dir());
        assert_eq!(store.default_account().unwrap().unwrap().auth, saved.auth);
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
    fn token_refresh_updates_the_recorded_plan() {
        let mut record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_plan", "plan@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        assert_eq!(record.plan_type.as_deref(), Some("plus"));
        // 刷新回来的令牌写的是当前套餐：降级后账号记录不能停在 plus。
        let downgraded = unsigned_jwt(json!({
            "email": "plan@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_plan",
                "chatgpt_plan_type": "free",
            }
        }));
        apply_token_response(
            &mut record,
            &json!({"access_token": "access-acct_plan-refreshed", "id_token": downgraded}),
        )
        .unwrap();
        assert_eq!(record.plan_type.as_deref(), Some("free"));
    }

    #[test]
    fn usage_plan_write_back_only_touches_the_plan() {
        let directory = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(directory.path());
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_live", "live@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&record).unwrap();

        assert!(store.update_plan_type(&record.id, Some("Pro")).unwrap());
        // 大小写不同是同一套餐，不重复落盘。
        assert!(!store.update_plan_type(&record.id, Some("pro")).unwrap());
        // 空值表示这次没有解析出套餐，保留已有结果。
        assert!(!store.update_plan_type(&record.id, Some("  ")).unwrap());
        assert!(
            !store
                .update_plan_type("acct_missing", Some("free"))
                .unwrap()
        );

        let stored = store.get(&record.id).unwrap().unwrap();
        assert_eq!(stored.plan_type.as_deref(), Some("Pro"));
        assert_eq!(stored.auth, record.auth, "凭据不受套餐回写影响");
    }

    #[test]
    fn stale_credential_update_does_not_restore_an_old_plan() {
        let directory = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(directory.path());
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_race", "race@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&record).unwrap();
        // 额度接口先写入了当前套餐。
        assert!(store.update_plan_type(&record.id, Some("free")).unwrap());
        // 随后到达的令牌刷新用的是刷新前的快照，套餐还是 plus。
        let mut refreshed = record.clone();
        apply_token_response(&mut refreshed, &json!({"access_token": "access-race-new"})).unwrap();
        assert_eq!(refreshed.plan_type.as_deref(), Some("plus"));
        let committed = store
            .update_credentials_if_current(&record, &refreshed)
            .unwrap()
            .unwrap();
        assert_eq!(committed.auth, refreshed.auth);
        assert_eq!(committed.plan_type.as_deref(), Some("free"));
    }

    #[test]
    fn codex_refresh_adopts_the_current_plan() {
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_sync", "sync@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        store.upsert(&record).unwrap();
        store.set_default_account_id(Some("acct_sync")).unwrap();
        // Codex 自己重登之后账号已经换成免费套餐。
        let mut downgraded = chatgpt_auth("acct_sync", "sync@example.com", "2026-02-01T00:00:00Z");
        downgraded["tokens"]["access_token"] = json!("access-synced");
        downgraded["tokens"]["id_token"] = json!(unsigned_jwt(json!({
            "email": "sync@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_sync",
                "chatgpt_plan_type": "free",
            }
        })));
        fs::write(
            home.path().join("auth.json"),
            serde_json::to_vec(&downgraded).unwrap(),
        )
        .unwrap();

        store.sync_default_from_codex_home(home.path()).unwrap();
        let stored = store.get("acct_sync").unwrap().unwrap();
        assert_eq!(stored.plan_type.as_deref(), Some("free"));
        assert_eq!(stored.summary(None).plan_type.as_deref(), Some("free"));
    }

    #[tokio::test]
    async fn callback_listener_rejects_occupied_port() {
        let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let error = bind_callback_listener(port).await.unwrap_err();
        assert!(error.to_string().contains("已被占用"));
        assert!(error.to_string().contains("固定回调地址"));
        assert!(error.to_string().contains(&format!("127.0.0.1:{port}")));
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

    fn manual_record(input: &str) -> OfficialAccountRecord {
        match parse_manual_official_credential(input).unwrap() {
            ManualOfficialCredential::Record(record) => *record,
            ManualOfficialCredential::RefreshToken(_) => panic!("应直接保存登录文档"),
        }
    }

    fn manual_refresh(input: &str) -> String {
        match parse_manual_official_credential(input).unwrap() {
            ManualOfficialCredential::RefreshToken(token) => token,
            ManualOfficialCredential::Record(_) => panic!("应保留 Refresh Token 以便换票"),
        }
    }

    fn manual_error(input: &str) -> String {
        match parse_manual_official_credential(input) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("应拒绝这份输入"),
        }
    }

    #[test]
    fn manual_input_keeps_oauth_json_and_exchanges_only_a_refresh_token() {
        let mut auth = chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z");
        auth["custom_flag"] = json!(true);
        let stored = manual_record(&serde_json::to_string(&auth).unwrap());
        assert_eq!(stored.id, "acct_1");
        assert_eq!(stored.auth["custom_flag"], json!(true));
        assert_eq!(
            stored.auth["tokens"]["access_token"],
            json!("access-acct_1")
        );
        assert_eq!(
            stored.auth["tokens"]["refresh_token"],
            json!("refresh-acct_1")
        );
        assert_eq!(stored.auth["last_refresh"], json!("2026-01-01T00:00:00Z"));

        let token = "rt_abcdefghijklmnopqrstuvwxyz";
        assert_eq!(manual_refresh(token), token);
        assert_eq!(manual_refresh(&format!("Bearer\n{token}  ")), token);
        assert_eq!(manual_refresh(&format!("\"{token}\"")), token);
        assert_eq!(
            manual_refresh(&json!({ "tokens": { "refreshToken": token } }).to_string()),
            token
        );
        assert_eq!(
            refresh_token_grant(token)["grant_type"],
            json!("refresh_token")
        );
        assert_eq!(
            refresh_token_grant(token)["client_id"],
            json!(OAUTH_CLIENT_ID)
        );
    }

    #[test]
    fn manual_oauth_json_accepts_flat_token_and_wrapped_account_files() {
        let live_exp = unix_timestamp() + 7_200;
        let access = unsigned_jwt(json!({
            "exp": live_exp,
            "email": "flat@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_flat",
                "chatgpt_plan_type": "pro",
            }
        }));
        let flat = manual_record(
            &json!({
                "access_token": access,
                "refresh_token": "refresh-flat",
                "id_token": access,
                "account_id": "acct_flat",
            })
            .to_string(),
        );
        assert_eq!(flat.id, "acct_flat");
        assert_eq!(flat.email.as_deref(), Some("flat@example.com"));
        assert_eq!(flat.plan_type.as_deref(), Some("pro"));
        assert_eq!(flat.auth["tokens"]["refresh_token"], json!("refresh-flat"));
        assert!(flat.last_refresh().is_some());

        let wrapped_access = unsigned_jwt(json!({ "exp": live_exp }));
        let wrapped = format!(
            "```json\n{}\n```",
            json!({
                "email": "wrap@example.com",
                "accountId": "acct_wrap",
                "planType": "team",
                "auth": {
                    "tokens": {
                        "access_token": wrapped_access,
                        "refresh_token": "refresh-wrap",
                    }
                }
            })
        );
        let wrapped = manual_record(&wrapped);
        assert_eq!(wrapped.id, "acct_wrap");
        assert_eq!(wrapped.email.as_deref(), Some("wrap@example.com"));
        assert_eq!(wrapped.plan_type.as_deref(), Some("team"));
        assert_eq!(wrapped.auth["tokens"]["account_id"], json!("acct_wrap"));
        assert_eq!(
            wrapped.auth["tokens"]["refresh_token"],
            json!("refresh-wrap")
        );
        assert!(wrapped.last_refresh().is_some());
    }

    #[test]
    fn manual_input_rejects_secrets_that_are_not_refresh_tokens_without_echoing_them() {
        let secret = "rt_super_secret_value_should_not_leak";
        let broken = format!("{{\"refresh_token\":\"{secret}\"");
        let message = manual_error(&broken);
        assert!(message.contains("格式无效"));
        assert!(!message.contains(secret));

        let jwt = unsigned_jwt(json!({"sub": "user"}));
        assert!(manual_error(&jwt).contains("Access Token"));
        assert!(manual_error("sk-abcdefghijklmnopqrstuvwxyz").contains("API Key"));
        assert!(manual_error("[]").contains("对象"));
        assert!(manual_error("   ").contains("请粘贴"));
        assert!(manual_error(&"x".repeat(MAX_MANUAL_CREDENTIAL_BYTES + 1)).contains("过长"));
        let expired = unsigned_jwt(json!({
            "exp": unix_timestamp().saturating_sub(30),
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_old" }
        }));
        let expired = manual_record(
            &json!({
                "access_token": expired,
                "refresh_token": "refresh-old",
            })
            .to_string(),
        );
        assert!(expired.last_refresh().is_none());
        assert!(
            manual_error(
                &json!({
                    "auth_mode": "apikey",
                    "tokens": {
                        "access_token": "x",
                        "refresh_token": "refresh-value-long-enough",
                    }
                })
                .to_string(),
            )
            .contains("ChatGPT")
        );
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

    #[test]
    fn only_invalid_grant_marks_the_account_invalid() {
        let invalid = official_account_invalid_from_body(
            br#"{"error":"invalid_grant","error_description":"refresh token revoked"}"#,
        )
        .expect("invalid_grant 表示刷新令牌已被撤销");
        assert!(invalid.reason().contains("已撤销"));
        assert_eq!(invalid.detail(), Some("refresh token revoked"));

        // 请求错误、限流、服务端故障和网络错误都不代表账号失效。
        for body in [
            br#"{"error":"invalid_request","error_description":"missing scope"}"#.as_slice(),
            br#"{"error":"temporarily_unavailable"}"#.as_slice(),
            br#"{"status":500}"#.as_slice(),
            b"<html>bad gateway</html>".as_slice(),
        ] {
            assert!(
                official_account_invalid_from_body(body).is_none(),
                "非 invalid_grant 响应不得标记账号失效：{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn invalid_state_round_trips_through_the_store_and_clears_on_refresh() {
        let dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let mut record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        assert!(!record.summary(None).invalid);

        record.mark_invalid("官方已撤销该账号的登录凭据，需要重新添加账号");
        store.upsert(&record).unwrap();
        let summary = store.summaries().unwrap().remove(0);
        assert!(summary.invalid);
        assert_eq!(
            summary.invalid_reason.as_deref(),
            Some("官方已撤销该账号的登录凭据，需要重新添加账号")
        );

        // Codex 用同一个账号成功刷新后，失效标记随之清除。
        let mut refreshed = chatgpt_auth("acct_1", "a@example.com", "2026-02-01T00:00:00Z");
        refreshed["tokens"]["access_token"] = json!("access-new");
        fs::write(
            home.path().join("auth.json"),
            serde_json::to_vec(&refreshed).unwrap(),
        )
        .unwrap();
        store.set_default_account_id(Some("acct_1")).unwrap();
        store.sync_default_from_codex_home(home.path()).unwrap();
        let stored = store.get("acct_1").unwrap().unwrap();
        assert!(!stored.invalid());
        assert!(stored.invalid_since.is_none());
    }

    #[test]
    fn live_access_token_check_excludes_expired_and_opaque_tokens() {
        let mut record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", "2026-01-01T00:00:00Z"),
            1,
        )
        .unwrap();
        record.auth["tokens"]["access_token"] = json!("opaque-token");
        assert!(!record.has_live_access_token());

        record.auth["tokens"]["access_token"] = json!(unsigned_jwt(json!({
            "exp": unix_timestamp() + 3600
        })));
        assert!(record.has_live_access_token());

        record.auth["tokens"]["access_token"] = json!(unsigned_jwt(json!({
            "exp": unix_timestamp().saturating_sub(3600)
        })));
        assert!(!record.has_live_access_token());
    }

    #[test]
    fn only_the_default_account_reads_the_codex_home_credential() {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let home = dir.path().join("codex-home");
        for id in ["acct_1", "acct_2"] {
            store
                .upsert(
                    &OfficialAccountRecord::from_auth(
                        chatgpt_auth(id, "a@example.com", "2026-01-01T00:00:00Z"),
                        1,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        store.set_default_account_id(Some("acct_2")).unwrap();

        assert_eq!(
            store.credential_path(&home, "acct_2"),
            home.join(CODEX_AUTH_FILE_NAME),
            "Codex refreshes the default account copy in place"
        );
        assert_eq!(
            store.credential_path(&home, "acct_1"),
            store.account_path("acct_1"),
            "idle accounts read the document Codey stored"
        );
    }

    #[test]
    fn stored_account_credentials_read_back_from_the_account_record() {
        let dir = TempDir::new().unwrap();
        let store = OfficialAccountStore::new(dir.path().join(ACCOUNTS_DIR_NAME));
        let home = dir.path().join("codex-home");
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
        store.set_default_account_id(Some("acct_1")).unwrap();

        let auth =
            crate::account_usage::read_official_auth(&store.credential_path(&home, "acct_2"))
                .expect("非默认账号的线路与额度都从自己的账号记录读取凭据");
        assert_eq!(auth.access_token, "access-acct_2");
        assert_eq!(auth.account_id.as_deref(), Some("acct_2"));
    }

    #[test]
    fn tokens_inside_the_expiry_margin_are_refreshed() {
        let mut record = OfficialAccountRecord::from_auth(
            chatgpt_auth("acct_1", "a@example.com", &rfc3339_now()),
            1,
        )
        .unwrap();
        record.auth["tokens"]["access_token"] =
            json!(unsigned_jwt(json!({ "exp": unix_timestamp() + 60 })));
        assert!(
            needs_token_refresh(&record),
            "a token that lapses inside the margin is refreshed"
        );

        record.auth["tokens"]["access_token"] = json!(unsigned_jwt(
            json!({ "exp": unix_timestamp() + 12 * 60 * 60 })
        ));
        assert!(
            !needs_token_refresh(&record),
            "a freshly stored long-lived token is left alone"
        );
    }
}
