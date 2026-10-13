use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use reqwest::{Client, StatusCode, header::ACCEPT};
use serde::{Deserialize, Serialize};
use serde_json::Value;

static LAST_GOOD_USAGE_ENDPOINT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

const CODEX_AUTH_FILE_NAME: &str = "auth.json";
const USAGE_ENDPOINTS: [&str; 2] = [
    "https://chatgpt.com/backend-api/wham/usage",
    "https://chatgpt.com/backend-api/api/codex/usage",
];
const ACCOUNT_USAGE_CACHE_TTL: Duration = Duration::from_secs(60);
const ACCOUNT_USAGE_FAILURE_BACKOFF_INITIAL: Duration = Duration::from_secs(60);
const ACCOUNT_USAGE_FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);
const ACCOUNT_USAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ACCOUNT_USAGE_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_JWT_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_JWT_PAYLOAD_ENCODED_BYTES: usize = 96 * 1024;
// Local-router requests only need eventual auth-file invalidation. Rechecking once per
// second avoids filesystem work on every request while keeping account switches prompt.
const OFFICIAL_AUTH_REVALIDATE_TTL: Duration = Duration::from_secs(1);
/// Upper bound for the per-account usage caches. The account store is far
/// smaller than this; the limit only stops an unexpected caller from growing
/// the map without bound.
const MAX_ACCOUNT_USAGE_CACHES: usize = 24;

#[derive(Debug, Clone)]
pub(crate) struct OfficialAuth {
    pub(crate) access_token: String,
    pub(crate) account_id: Option<String>,
    /// 读盘时解析一次，供路由比较新旧令牌时避免重复解码 JWT。
    pub(crate) issued_at: Option<u64>,
}

impl PartialEq for OfficialAuth {
    fn eq(&self, other: &Self) -> bool {
        self.access_token == other.access_token && self.account_id == other.account_id
    }
}

/// 额度查询结果里表示凭据被官方拒绝的 reason 值，调用方据此标记账号失效。
pub(crate) const USAGE_REASON_CREDENTIAL_REJECTED: &str = "official_account_credential_rejected";

/// 官方额度接口以 401 拒绝当前凭据。请求方会结合本地令牌是否仍在有效期
/// 内判断账号是否失效，避免把尚未刷新的过期令牌误判成账号被撤销。
#[derive(Debug)]
pub(crate) struct AccountUsageUnauthorized;

impl std::fmt::Display for AccountUsageUnauthorized {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "官方额度接口拒绝当前账号的凭据，登录状态可能已失效"
        )
    }
}

impl std::error::Error for AccountUsageUnauthorized {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OfficialAuthFingerprint {
    len: u64,
    modified: SystemTime,
}

pub(crate) fn official_auth_fingerprint(path: &Path) -> Option<OfficialAuthFingerprint> {
    let metadata = fs::metadata(path).ok()?;
    Some(OfficialAuthFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok()?,
    })
}

#[derive(Debug, Default)]
pub(crate) struct OfficialAuthCache {
    cached: Option<std::result::Result<Arc<OfficialAuth>, String>>,
    fingerprint: Option<OfficialAuthFingerprint>,
    expires_at: Option<Instant>,
    last_used: u64,
}

impl OfficialAuthCache {
    /// Returns the cached auth (or cached read failure) without reading the file
    /// contents. After the short TTL, a matching size/mtime fingerprint still
    /// reuses the cache so unchanged credentials skip JSON parsing.
    pub(crate) fn get(
        &mut self,
        path: &Path,
        now: Instant,
    ) -> Option<std::result::Result<Arc<OfficialAuth>, String>> {
        if self.expires_at.is_some_and(|expires_at| now < expires_at) {
            return self.cached.clone();
        }
        if self.cached.is_none() || self.fingerprint.is_none() {
            return None;
        }
        if official_auth_fingerprint(path) != self.fingerprint {
            return None;
        }
        self.expires_at = Some(now + OFFICIAL_AUTH_REVALIDATE_TTL);
        self.cached.clone()
    }

    /// Stores an auth-file read result for the short revalidation interval.
    pub(crate) fn store(
        &mut self,
        path: &Path,
        result: Result<OfficialAuth>,
        now: Instant,
    ) -> Result<Arc<OfficialAuth>> {
        let result = result.map(Arc::new).map_err(|error| error.to_string());
        self.fingerprint = official_auth_fingerprint(path);
        self.cached = Some(result.clone());
        self.expires_at = Some(now + OFFICIAL_AUTH_REVALIDATE_TTL);
        result.map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    fn read_at(&mut self, path: &Path, now: Instant) -> Result<OfficialAuth> {
        match self.get(path, now) {
            Some(result) => result
                .map(|auth| (*auth).clone())
                .map_err(anyhow::Error::msg),
            None => self
                .store(path, read_official_auth(path), now)
                .map(|auth| (*auth).clone()),
        }
    }
}

/// Credential reads of several official accounts at once. Every account reads
/// its own document, so one account never answers with another account's
/// token.
#[derive(Debug, Default)]
pub(crate) struct OfficialAuthCaches {
    caches: HashMap<String, OfficialAuthCache>,
    usage_clock: u64,
}

impl OfficialAuthCaches {
    pub(crate) fn for_path(&mut self, auth_path: &Path) -> &mut OfficialAuthCache {
        let key = auth_path.to_string_lossy().into_owned();
        if !self.caches.contains_key(&key) && self.caches.len() >= MAX_ACCOUNT_USAGE_CACHES {
            evict_least_recently_used(&mut self.caches, |cache| cache.last_used);
        }
        self.usage_clock = self.usage_clock.saturating_add(1);
        let last_used = self.usage_clock;
        let cache = self.caches.entry(key).or_default();
        cache.last_used = last_used;
        cache
    }
}

/// 缓存达到上限时只淘汰最久未使用的条目，其他账号的额度结果继续命中。
fn evict_least_recently_used<T>(caches: &mut HashMap<String, T>, last_used: impl Fn(&T) -> u64) {
    let Some(key) = caches
        .iter()
        .min_by_key(|(_, value)| last_used(value))
        .map(|(key, _)| key.clone())
    else {
        return;
    };
    caches.remove(&key);
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsageSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<AccountUsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<AccountUsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits: Option<AccountCredits>,
    pub fetched_at: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsageWindow {
    pub used_percent: f64,
    pub window_minutes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
}

#[derive(Debug, Default)]
pub struct AccountUsageCache {
    snapshot: Option<AccountUsageSnapshot>,
    expires_at: Option<Instant>,
    consecutive_failures: u32,
    retry: Option<(Instant, String)>,
    auth_fingerprint_initialized: bool,
    auth_fingerprint: Option<OfficialAuthFingerprint>,
    auth_generation: u64,
    last_used: u64,
}

/// Usage snapshots for several official accounts at once. Each credential file
/// keeps its own snapshot, TTL, failure backoff and auth fingerprint, so one
/// account being unavailable never hides another account's usage.
#[derive(Debug, Default)]
pub struct AccountUsageCaches {
    caches: HashMap<String, AccountUsageCache>,
    usage_clock: u64,
}

impl AccountUsageCaches {
    pub(crate) fn for_auth_path(&mut self, auth_path: &Path) -> &mut AccountUsageCache {
        let key = auth_path.to_string_lossy().into_owned();
        if !self.caches.contains_key(&key) && self.caches.len() >= MAX_ACCOUNT_USAGE_CACHES {
            evict_least_recently_used(&mut self.caches, |cache| cache.last_used);
        }
        self.usage_clock = self.usage_clock.saturating_add(1);
        let last_used = self.usage_clock;
        let cache = self.caches.entry(key).or_default();
        cache.last_used = last_used;
        cache
    }

    pub(crate) fn for_codex_home(&mut self, codex_home: &Path) -> &mut AccountUsageCache {
        let auth_path = codex_home.join(CODEX_AUTH_FILE_NAME);
        self.for_auth_path(&auth_path)
    }
}

/// Credential document of the account Codex itself is logged in as.
pub(crate) fn codex_auth_path(codex_home: &Path) -> std::path::PathBuf {
    codex_home.join(CODEX_AUTH_FILE_NAME)
}

impl AccountUsageCache {
    pub(crate) fn store_displayed_snapshot(
        &mut self,
        auth_path: &Path,
        auth_generation: u64,
        snapshot: AccountUsageSnapshot,
    ) -> Result<()> {
        self.observe_auth_fingerprint(official_auth_fingerprint(auth_path));
        if self.auth_generation != auth_generation {
            bail!("官方登录状态已变化，请重新读取额度");
        }
        let now = unix_timestamp();
        if snapshot.fetched_at == 0
            || snapshot.fetched_at > now + 1
            || now.saturating_sub(snapshot.fetched_at) > 120
            || snapshot.primary.is_none() && snapshot.secondary.is_none()
            || [&snapshot.primary, &snapshot.secondary]
                .into_iter()
                .flatten()
                .any(|window| {
                    !window.used_percent.is_finite()
                        || !(0.0..=100.0).contains(&window.used_percent)
                        || window.window_minutes == 0
                })
        {
            bail!("同步的官方额度数据无效或已过期");
        }
        if self
            .snapshot
            .as_ref()
            .is_none_or(|current| current.fetched_at <= snapshot.fetched_at)
        {
            self.record_success(snapshot, Instant::now());
        }
        Ok(())
    }

    fn valid_weekly_snapshot(&self) -> Option<AccountUsageSnapshot> {
        let snapshot = self.snapshot.as_ref()?;
        let now = unix_timestamp();
        [&snapshot.primary, &snapshot.secondary]
            .into_iter()
            .flatten()
            .find(|window| {
                window.window_minutes == 10080
                    && (0.0..=100.0).contains(&window.used_percent)
                    && window.resets_at.is_some_and(|end| {
                        end > now
                            && snapshot.fetched_at < end
                            && snapshot.fetched_at > end.saturating_sub(604800)
                            && snapshot.fetched_at <= now + 1
                    })
            })?;
        Some(snapshot.clone())
    }

    /// Refreshes the snapshot that belongs to one credential document. Callers
    /// that serve several official accounts pass the matching file so every
    /// account keeps an independent cache entry.
    pub(crate) async fn fetch_at(
        &mut self,
        auth_path: &Path,
        force_refresh: bool,
        upstream_proxy: Option<&str>,
    ) -> Result<AccountUsageSnapshot> {
        self.observe_auth_fingerprint(official_auth_fingerprint(auth_path));
        let generation = self.auth_generation;
        if force_refresh {
            self.expires_at = None;
        }
        if let Some(cached) = self.cached_result(Instant::now()) {
            return cached.map_err(anyhow::Error::msg);
        }

        // reqwest snapshots the current system proxy when a client is built. Rebuild the
        // dedicated usage client for each network refresh so proxy changes do not require
        // restarting Codey. Cached results still avoid unnecessary requests and rebuilds.
        // 官方线路配置了上游代理时，额度查询走同一出口，避免同一账号同时从
        // 两个地区访问。
        let result = match account_usage_http_client(upstream_proxy) {
            Ok(client) => fetch_official_account_usage(&client, auth_path).await,
            Err(error) => Err(error),
        };

        self.observe_auth_fingerprint(official_auth_fingerprint(auth_path));
        if generation != self.auth_generation {
            bail!("官方登录状态已变化，请重新读取额度");
        }
        match result {
            Ok(snapshot) => {
                self.record_success(snapshot.clone(), Instant::now());
                Ok(snapshot)
            }
            Err(error) => {
                // 只记录实际刷新失败；缓存命中不重复写入，相同错误由日志模块去重。
                // 使用顶层错误提示，不展开可能包含凭据或响应原文的错误链。
                crate::error_log::record_failure_with_metadata_async(
                    "official_account_usage_failed",
                    "query_official_account_usage",
                    error.to_string(),
                    crate::error_log::FailureMetadata {
                        stage: Some("account_usage.refresh".to_string()),
                        recoverable: Some(true),
                    },
                    serde_json::json!({
                        "forceRefresh": force_refresh,
                        "consecutiveFailures": self.consecutive_failures.saturating_add(1),
                    }),
                )
                .await;
                self.record_failure(error.to_string(), Instant::now());
                Err(error)
            }
        }
    }

    fn cached_result(
        &self,
        now: Instant,
    ) -> Option<std::result::Result<AccountUsageSnapshot, String>> {
        if self.expires_at.is_some_and(|expires_at| now < expires_at) {
            return self.snapshot.clone().map(Ok);
        }
        if let Some((retry_at, error)) = &self.retry
            && now < *retry_at
        {
            return Some(Err(error.clone()));
        }
        None
    }

    fn record_success(&mut self, snapshot: AccountUsageSnapshot, now: Instant) {
        self.snapshot = Some(snapshot);
        self.expires_at = Some(now + ACCOUNT_USAGE_CACHE_TTL);
        self.consecutive_failures = 0;
        self.retry = None;
    }

    fn record_failure(&mut self, error: String, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.retry = Some((
            now + account_usage_failure_backoff(self.consecutive_failures),
            error,
        ));
    }

    fn observe_auth_fingerprint(&mut self, fingerprint: Option<OfficialAuthFingerprint>) {
        if !self.auth_fingerprint_initialized {
            self.auth_generation += 1;
            self.auth_fingerprint_initialized = true;
            self.auth_fingerprint = fingerprint;
            return;
        }
        if self.auth_fingerprint == fingerprint {
            return;
        }
        self.auth_fingerprint = fingerprint;
        self.auth_generation += 1;
        self.snapshot = None;
        self.expires_at = None;
        self.consecutive_failures = 0;
        self.retry = None;
    }
}

pub(crate) async fn query_snapshot(
    cache: &mut AccountUsageCache,
    home: &Path,
    force_refresh: bool,
    upstream_proxy: Option<&str>,
) -> Value {
    let auth_path = home.join(CODEX_AUTH_FILE_NAME);
    query_snapshot_at(cache, &auth_path, force_refresh, upstream_proxy).await
}

/// Reads one credential document's usage, keeping the snapshot, TTL and
/// failure backoff of that account only.
pub(crate) async fn query_snapshot_at(
    cache: &mut AccountUsageCache,
    auth_path: &Path,
    force_refresh: bool,
    upstream_proxy: Option<&str>,
) -> Value {
    let result = if force_refresh {
        cache.fetch_at(auth_path, true, upstream_proxy).await
    } else {
        cache.fetch_at(auth_path, false, upstream_proxy).await
    };
    let mut value = match result {
        Ok(snapshot) => {
            let mut value = serde_json::to_value(snapshot)
                .expect("account usage snapshots must be JSON-serializable");
            value["status"] = Value::String("ok".into());
            value
        }
        Err(error) => match cache.valid_weekly_snapshot() {
            Some(snapshot) => {
                let mut value =
                    serde_json::to_value(snapshot).expect("serializable usage snapshot");
                value["status"] = Value::String("ok".into());
                value["stale"] = Value::Bool(true);
                value["message"] = Value::String(
                    "额度刷新失败，正在使用本周上次成功获取的数据，统计截止时间保持不变。".into(),
                );
                // 401 是凭据被官方明确拒绝，不能随历史快照吞掉：reason 透传给
                // 调用方标失效，界面才不会一直定格在「上次成功数据」。
                if error.downcast_ref::<AccountUsageUnauthorized>().is_some() {
                    value["reason"] = Value::String(USAGE_REASON_CREDENTIAL_REJECTED.to_string());
                }
                value
            }
            None => {
                let mut value =
                    serde_json::json!({"status": "error", "message": error.to_string()});
                if error.downcast_ref::<AccountUsageUnauthorized>().is_some() {
                    value["reason"] = Value::String(USAGE_REASON_CREDENTIAL_REJECTED.to_string());
                }
                value
            }
        },
    };
    value["authGeneration"] = cache.auth_generation.into();
    value
}

fn account_usage_http_client(upstream_proxy: Option<&str>) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(ACCOUNT_USAGE_CONNECT_TIMEOUT);
    if let Some(proxy) = upstream_proxy
        .map(str::trim)
        .filter(|proxy| !proxy.is_empty())
    {
        builder = builder.proxy(reqwest::Proxy::all(proxy).context("官方线路的上游代理地址无效")?);
    }
    builder.build().context("创建官方额度网络客户端失败")
}

fn account_usage_failure_backoff(consecutive_failures: u32) -> Duration {
    let shift = consecutive_failures.saturating_sub(1).min(3);
    let seconds = ACCOUNT_USAGE_FAILURE_BACKOFF_INITIAL
        .as_secs()
        .saturating_mul(1_u64 << shift)
        .min(ACCOUNT_USAGE_FAILURE_BACKOFF_MAX.as_secs());
    Duration::from_secs(seconds)
}

pub async fn fetch_official_account_usage(
    client: &Client,
    auth_path: &Path,
) -> Result<AccountUsageSnapshot> {
    let auth_path = auth_path.to_path_buf();
    let auth = tokio::task::spawn_blocking(move || read_official_auth(&auth_path))
        .await
        .context("读取 Codex 官方登录信息任务异常退出")??;
    let mut last_error = None;

    // 从上次成功的端点开始轮询，失败仍会回退到完整列表，结果不变但稳定
    // 状态下每次刷新只发一个请求。
    let start = LAST_GOOD_USAGE_ENDPOINT.load(std::sync::atomic::Ordering::Relaxed);
    for offset in 0..USAGE_ENDPOINTS.len() {
        let index = (start + offset) % USAGE_ENDPOINTS.len();
        let endpoint = USAGE_ENDPOINTS[index];
        let mut request = client
            .get(endpoint)
            .timeout(Duration::from_secs(8))
            .header(ACCEPT, "application/json")
            .header("user-agent", "codex_cli_rs")
            .bearer_auth(&auth.access_token);
        if let Some(account_id) = auth.account_id.as_deref() {
            request = request.header("chatgpt-account-id", account_id);
        }

        let response = request.send().await.with_context(|| "官方额度请求失败")?;
        let status = response.status();
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            last_error = Some(format!("官方额度接口返回 {status}"));
            continue;
        }
        if !status.is_success() {
            if status == StatusCode::UNAUTHORIZED {
                return Err(anyhow::Error::new(AccountUsageUnauthorized));
            }
            bail!("官方额度接口返回 {status}");
        }

        let response = crate::http_response::read_bounded_body(
            response,
            MAX_ACCOUNT_USAGE_RESPONSE_BYTES,
            "官方额度响应",
        )
        .await?;
        let payload =
            serde_json::from_slice::<Value>(&response).with_context(|| "官方额度响应格式无效")?;
        LAST_GOOD_USAGE_ENDPOINT.store(index, std::sync::atomic::Ordering::Relaxed);
        return parse_account_usage(&payload, unix_timestamp());
    }

    bail!(
        "{}",
        last_error.unwrap_or_else(|| "未找到可用的官方额度接口".to_string())
    )
}

pub(crate) fn read_official_auth(path: &Path) -> Result<OfficialAuth> {
    let bytes = fs::read(path).with_context(|| "未找到 Codex 官方登录信息")?;
    let value: Value = serde_json::from_slice(&bytes).with_context(|| "Codex 登录信息格式无效")?;
    // 非默认账号的凭据保存在 Codey 的账号记录里，auth.json 原文位于 auth
    // 字段；两种文档都在这里解析，账号线路和额度查询共用同一条读取路径。
    let Some(auth) = chatgpt_auth_document(&value) else {
        bail!("当前不是 ChatGPT 官方账号登录");
    };

    let tokens = auth
        .get("tokens")
        .and_then(Value::as_object)
        .context("Codex 官方登录令牌缺失")?;
    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .context("Codex 官方访问令牌缺失")?
        .to_string();
    let account_id = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            ["id_token", "access_token"].iter().find_map(|key| {
                tokens
                    .get(*key)
                    .and_then(Value::as_str)
                    .and_then(account_id_from_jwt)
            })
        });

    Ok(OfficialAuth {
        issued_at: crate::official_accounts::access_token_issued_at(&access_token),
        access_token,
        account_id,
    })
}

/// 官方登录文档在磁盘上有两种形态：Codex 直接维护的 auth.json，以及 Codey
/// 保存的账号记录（登录文档嵌套在 auth 字段里）。只有 ChatGPT 登录才返回。
fn chatgpt_auth_document(value: &Value) -> Option<&Value> {
    fn is_chatgpt_login(value: &Value) -> bool {
        value
            .get("auth_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("chatgpt"))
    }
    if is_chatgpt_login(value) {
        return Some(value);
    }
    value.get("auth").filter(|nested| is_chatgpt_login(nested))
}

fn account_id_from_jwt(token: &str) -> Option<String> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() || payload.len() > MAX_JWT_PAYLOAD_ENCODED_BYTES {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    if decoded.len() > MAX_JWT_PAYLOAD_BYTES {
        return None;
    }
    let claims = serde_json::from_slice::<Value>(&decoded).ok()?;
    account_id_from_claims(&claims)
}

fn account_id_from_claims(claims: &Value) -> Option<String> {
    let auth_claims = claims.get("https://api.openai.com/auth");
    string_field(
        claims,
        &[
            "chatgpt_account_id",
            "https://api.openai.com/auth.chatgpt_account_id",
        ],
    )
    .or_else(|| auth_claims.and_then(|value| string_field(value, &["chatgpt_account_id"])))
    .or_else(|| organization_account_id(claims))
    .or_else(|| auth_claims.and_then(organization_account_id))
    .filter(|account_id| account_id.len() <= 1024)
}

fn organization_account_id(value: &Value) -> Option<String> {
    value
        .get("organizations")?
        .as_array()?
        .iter()
        .find_map(|organization| string_field(organization, &["id"]))
}

fn parse_account_usage(value: &Value, fetched_at: u64) -> Result<AccountUsageSnapshot> {
    let snapshot = [
        "rate_limits",
        "rateLimits",
        "rate_limit",
        "rateLimit",
        "rate_limit_status",
        "rateLimitStatus",
    ]
    .iter()
    .find_map(|key| value.get(*key))
    .unwrap_or(value);

    let primary = parse_window(
        snapshot,
        &["primary", "primary_window", "primaryWindow"],
        fetched_at,
    );
    let secondary = parse_window(
        snapshot,
        &["secondary", "secondary_window", "secondaryWindow"],
        fetched_at,
    );
    let credits = parse_credits(snapshot).or_else(|| parse_credits(value));
    if primary.is_none() && secondary.is_none() && credits.is_none() {
        bail!("官方额度响应中没有可展示的额度信息");
    }

    let plan_type = string_field(value, &["plan_type", "planType"])
        .or_else(|| string_field(snapshot, &["plan_type", "planType"]));
    Ok(AccountUsageSnapshot {
        plan_type,
        primary,
        secondary,
        credits,
        fetched_at,
    })
}

fn parse_window(value: &Value, keys: &[&str], fetched_at: u64) -> Option<AccountUsageWindow> {
    let window = keys.iter().find_map(|key| value.get(*key))?;
    let used_percent = number_field(window, &["used_percent", "usedPercent"]).or_else(|| {
        number_field(window, &["remaining_percent", "remainingPercent"])
            .map(|remaining| 100.0 - remaining)
    })?;
    let window_minutes = u64_field(
        window,
        &[
            "window_minutes",
            "windowMinutes",
            "window_duration_mins",
            "windowDurationMins",
        ],
    )
    .or_else(|| {
        u64_field(window, &["limit_window_seconds", "limitWindowSeconds"])
            .map(|seconds| seconds.div_ceil(60))
    })?;
    let resets_at = u64_field(window, &["resets_at", "resetsAt", "reset_at", "resetAt"])
        .map(normalize_timestamp)
        .or_else(|| {
            u64_field(window, &["reset_after_seconds", "resetAfterSeconds"])
                .map(|seconds| fetched_at.saturating_add(seconds))
        });

    Some(AccountUsageWindow {
        used_percent: used_percent.clamp(0.0, 100.0),
        window_minutes,
        resets_at,
    })
}

fn parse_credits(value: &Value) -> Option<AccountCredits> {
    let credits = value.get("credits")?;
    let unlimited = bool_field(credits, &["unlimited"]).unwrap_or(false);
    let balance = string_or_number_field(credits, &["balance"]);
    let has_credits = bool_field(credits, &["has_credits", "hasCredits"])
        .unwrap_or(unlimited || balance.as_deref().is_some_and(|balance| balance != "0"));
    Some(AccountCredits {
        has_credits,
        unlimited,
        balance,
    })
}

fn number_field(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|field| field.as_f64().or_else(|| field.as_str()?.parse().ok()))
    })
}

fn u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    number_field(value, keys)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round() as u64)
}

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn string_or_number_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let field = value.get(*key)?;
        if let Some(value) = field
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
        field
            .as_f64()
            .filter(|value| value.is_finite())
            .map(|value| value.to_string())
    })
}

fn normalize_timestamp(timestamp: u64) -> u64 {
    if timestamp > 10_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    }
}

fn unix_timestamp() -> u64 {
    crate::fs_util::timestamp_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn unsigned_jwt(payload: Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        format!("{header}.{payload}.signature")
    }

    fn sample_snapshot() -> AccountUsageSnapshot {
        AccountUsageSnapshot {
            plan_type: Some("plus".to_string()),
            primary: None,
            secondary: None,
            credits: Some(AccountCredits {
                has_credits: true,
                unlimited: false,
                balance: Some("10".to_string()),
            }),
            fetched_at: 1_700_000_000,
        }
    }

    #[test]
    fn successful_snapshots_are_reused_only_within_the_ttl() {
        let started_at = Instant::now();
        let snapshot = sample_snapshot();
        let mut cache = AccountUsageCache::default();
        cache.record_success(snapshot.clone(), started_at);

        assert_eq!(
            cache
                .cached_result(started_at + ACCOUNT_USAGE_CACHE_TTL - Duration::from_millis(1))
                .unwrap()
                .unwrap(),
            snapshot
        );
        assert!(
            cache
                .cached_result(started_at + ACCOUNT_USAGE_CACHE_TTL)
                .is_none()
        );
    }

    #[tokio::test]
    async fn displayed_usage_is_shared_with_log_window_and_survives_refresh_failure() {
        let home = crate::codex_config::codex_home();
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(AccountUsageCaches::default()));
        let mut snapshot = sample_snapshot();
        snapshot.fetched_at = unix_timestamp();
        snapshot.primary = Some(AccountUsageWindow {
            used_percent: 40.0,
            window_minutes: 10080,
            resets_at: Some(snapshot.fetched_at + 86400),
        });
        snapshot.secondary = None;
        shared
            .lock()
            .await
            .for_codex_home(home)
            .store_displayed_snapshot(&home.join(CODEX_AUTH_FILE_NAME), 1, snapshot.clone())
            .unwrap();
        let mut profile = crate::config::ProviderProfile::new("Official");
        profile.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
        profile.normalize();
        let config = crate::config::CodeyConfig {
            profiles: vec![profile],
            official_account_available_this_launch: true,
            ..Default::default()
        };
        let router = crate::local_router::LocalRouter::start_with_usage(&config, shared.clone())
            .await
            .unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for force in [false, true] {
            // Force-refresh must fail locally; this test never contacts the official service.
            shared
                .lock()
                .await
                .for_codex_home(home)
                .record_failure("offline".into(), Instant::now());
            let value: Value = client
                .post(format!(
                    "{}/codey/api/query_official_account_usage",
                    endpoint.base_url.trim_end_matches("/v1")
                ))
                .header("x-codey-router-token", &endpoint.token)
                .json(&serde_json::json!({"forceRefresh": force}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(value["status"], "ok", "{value}");
            assert_eq!(value["fetchedAt"], snapshot.fetched_at);
            assert_eq!(value["primary"]["usedPercent"], 40.0);
            assert_eq!(value["stale"].as_bool().unwrap_or(false), force);
        }
        router.stop().await.unwrap();
    }

    #[test]
    fn displayed_usage_rejects_old_account_and_invalid_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let mut cache = AccountUsageCache::default();
        let mut snapshot = sample_snapshot();
        snapshot.fetched_at = unix_timestamp();
        snapshot.primary = Some(AccountUsageWindow {
            used_percent: 40.0,
            window_minutes: 10080,
            resets_at: Some(snapshot.fetched_at + 86400),
        });
        snapshot.secondary = None;
        cache
            .store_displayed_snapshot(&directory.path().join("auth.json"), 1, snapshot.clone())
            .unwrap();
        assert!(cache.valid_weekly_snapshot().is_some());
        let mut invalid = snapshot.clone();
        invalid.primary.as_mut().unwrap().used_percent = 101.0;
        assert!(
            cache
                .store_displayed_snapshot(&directory.path().join("auth.json"), 1, invalid)
                .is_err()
        );
        std::fs::write(directory.path().join("auth.json"), "changed account").unwrap();
        assert!(
            cache
                .store_displayed_snapshot(&directory.path().join("auth.json"), 1, snapshot.clone())
                .is_err()
        );
        assert!(cache.valid_weekly_snapshot().is_none());
        snapshot.primary.as_mut().unwrap().resets_at = Some(unix_timestamp());
        cache.record_success(snapshot, Instant::now());
        assert!(cache.valid_weekly_snapshot().is_none());
    }

    #[tokio::test]
    async fn forced_usage_refresh_bypasses_success_and_preserves_failure_backoff() {
        let directory = tempfile::tempdir().unwrap();
        let mut cache = AccountUsageCache::default();
        cache.record_success(sample_snapshot(), Instant::now());
        let cached = query_snapshot(&mut cache, directory.path(), false, None).await;
        assert_eq!(cached["status"], "ok");
        assert_eq!(cached["fetchedAt"], 1_700_000_000_u64);
        // No auth file: a forced refresh must fetch instead of returning the snapshot.
        let failed = query_snapshot(&mut cache, directory.path(), true, None).await;
        assert_eq!(failed["status"], "error");
        assert!(failed["message"].as_str().unwrap().contains("官方登录信息"));
        assert_eq!(cache.consecutive_failures, 1);
        assert_eq!(
            query_snapshot(&mut cache, directory.path(), true, None).await,
            failed
        );
        assert_eq!(cache.consecutive_failures, 1);
    }

    #[test]
    fn failures_back_off_exponentially_and_success_resets_the_delay() {
        let mut cache = AccountUsageCache::default();
        let mut attempt_at = Instant::now();
        for expected_seconds in [60, 120, 240, 300, 300] {
            cache.record_failure("offline".to_string(), attempt_at);
            assert_eq!(
                cache.retry.as_ref().unwrap().0.duration_since(attempt_at),
                Duration::from_secs(expected_seconds)
            );
            assert_eq!(
                cache
                    .cached_result(attempt_at + Duration::from_secs(expected_seconds - 1))
                    .unwrap()
                    .unwrap_err(),
                "offline"
            );
            attempt_at += Duration::from_secs(expected_seconds);
        }

        cache.record_success(sample_snapshot(), attempt_at);
        assert_eq!(cache.consecutive_failures, 0);
        assert!(cache.retry.is_none());
    }

    #[test]
    fn reads_chatgpt_auth_without_exposing_other_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"token-value","account_id":"account-value","refresh_token":"do-not-copy"}}"#,
        )
        .unwrap();

        assert_eq!(
            read_official_auth(&path).unwrap(),
            OfficialAuth {
                access_token: "token-value".into(),
                account_id: Some("account-value".into()),
                issued_at: None,
            }
        );
    }

    #[test]
    fn reads_chatgpt_auth_from_a_stored_account_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("account.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "id": "account-value",
                "email": "a@example.com",
                "planType": "plus",
                "accountId": "account-value",
                "addedAt": 1,
                "auth": {
                    "auth_mode": "chatgpt",
                    "tokens": {
                        "access_token": "token-value",
                        "account_id": "account-value"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_official_auth(&path).unwrap(),
            OfficialAuth {
                access_token: "token-value".into(),
                account_id: Some("account-value".into()),
                issued_at: None,
            },
            "非默认账号的凭据保存在账号记录的 auth 字段里"
        );
    }

    #[test]
    fn rejects_stored_account_records_without_a_chatgpt_login() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("account.json");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "id": "account-value",
                "addedAt": 1,
                "auth": {
                    "auth_mode": "apikey",
                    "OPENAI_API_KEY": "sk"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(
            read_official_auth(&path)
                .unwrap_err()
                .to_string()
                .contains("不是 ChatGPT 官方账号登录")
        );
    }

    #[test]
    fn derives_account_id_from_jwt_without_overriding_an_explicit_value() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let id_token = unsigned_jwt(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-from-jwt"
            }
        }));
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "access_token": "access-token",
                    "id_token": id_token
                }
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_official_auth(&path).unwrap().account_id.as_deref(),
            Some("account-from-jwt")
        );

        let explicit = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "access-token",
                "account_id": "explicit-account",
                "id_token": unsigned_jwt(serde_json::json!({
                    "chatgpt_account_id": "account-from-jwt"
                }))
            }
        });
        fs::write(&path, serde_json::to_vec(&explicit).unwrap()).unwrap();
        assert_eq!(
            read_official_auth(&path).unwrap().account_id.as_deref(),
            Some("explicit-account")
        );
    }

    #[test]
    fn reads_access_token_issued_at_once_from_the_jwt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let access_token = unsigned_jwt(serde_json::json!({ "iat": 1_700_000_000_u64 }));
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "access_token": access_token, "account_id": "acct" }
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_official_auth(&path).unwrap().issued_at,
            Some(1_700_000_000)
        );
    }

    #[test]
    fn derives_account_id_from_access_token_organization_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let access_token = unsigned_jwt(serde_json::json!({
            "organizations": [{"id": "organization-account"}]
        }));
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {"access_token": access_token}
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_official_auth(&path).unwrap().account_id.as_deref(),
            Some("organization-account")
        );
    }

    #[test]
    fn official_auth_cache_refreshes_when_the_file_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"first","account_id":"acct-1"}}"#,
        )
        .unwrap();
        let mut cache = OfficialAuthCache::default();
        let now = Instant::now();
        assert_eq!(cache.read_at(&path, now).unwrap().access_token, "first");

        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"second-token","account_id":"acct-2"}}"#,
        )
        .unwrap();
        assert_eq!(
            cache
                .read_at(&path, now + Duration::from_millis(1))
                .unwrap()
                .access_token,
            "first"
        );

        let refreshed = cache
            .read_at(&path, now + OFFICIAL_AUTH_REVALIDATE_TTL)
            .unwrap();
        assert_eq!(refreshed.access_token, "second-token");
        assert_eq!(refreshed.account_id.as_deref(), Some("acct-2"));
    }

    #[test]
    fn official_auth_cache_reuses_matching_fingerprint_after_ttl() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"first","account_id":"acct-1"}}"#,
        )
        .unwrap();
        let mut cache = OfficialAuthCache::default();
        let now = Instant::now();
        assert_eq!(cache.read_at(&path, now).unwrap().access_token, "first");

        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"should-not-read"}}"#,
        )
        .unwrap();
        cache.fingerprint = official_auth_fingerprint(&path);
        assert_eq!(
            cache
                .get(&path, now + OFFICIAL_AUTH_REVALIDATE_TTL)
                .unwrap()
                .unwrap()
                .access_token,
            "first",
            "TTL 到期后只要 size/mtime 仍匹配就复用缓存，不再读盘解析"
        );
    }

    #[test]
    fn official_auth_cache_does_not_serve_a_removed_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        fs::write(
            &path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"first"}}"#,
        )
        .unwrap();
        let mut cache = OfficialAuthCache::default();
        let now = Instant::now();
        assert_eq!(cache.read_at(&path, now).unwrap().access_token, "first");

        fs::remove_file(&path).unwrap();
        assert!(
            cache
                .read_at(&path, now + OFFICIAL_AUTH_REVALIDATE_TTL)
                .is_err()
        );
    }

    #[test]
    fn usage_cache_clears_backoff_when_auth_changes() {
        let started_at = Instant::now();
        let mut cache = AccountUsageCache::default();
        let first = OfficialAuthFingerprint {
            len: 10,
            modified: UNIX_EPOCH + Duration::from_secs(1),
        };
        let second = OfficialAuthFingerprint {
            len: 11,
            modified: UNIX_EPOCH + Duration::from_secs(2),
        };
        cache.observe_auth_fingerprint(Some(first.clone()));
        cache.record_failure("expired token".to_string(), started_at);
        assert!(
            cache
                .cached_result(started_at + Duration::from_secs(1))
                .is_some()
        );

        cache.observe_auth_fingerprint(Some(first));
        assert!(cache.retry.is_some());
        cache.observe_auth_fingerprint(Some(second));
        assert!(cache.retry.is_none());
        assert_eq!(cache.consecutive_failures, 0);
        assert!(
            cache
                .cached_result(started_at + Duration::from_secs(1))
                .is_none()
        );
    }

    #[test]
    fn parses_official_rate_limit_snapshot() {
        let value = serde_json::json!({
            "rate_limits": {
                "primary": {
                    "used_percent": 15.0,
                    "window_minutes": 300,
                    "resets_at": 1_800_000_000
                },
                "secondary": {
                    "used_percent": 33.0,
                    "window_minutes": 10_080,
                    "resets_at": 1_800_500_000
                },
                "credits": {
                    "has_credits": false,
                    "unlimited": false,
                    "balance": "0"
                },
                "plan_type": "pro"
            }
        });

        let snapshot = parse_account_usage(&value, 1_700_000_000).unwrap();
        assert_eq!(snapshot.plan_type.as_deref(), Some("pro"));
        assert_eq!(snapshot.primary.unwrap().used_percent, 15.0);
        assert_eq!(snapshot.secondary.unwrap().window_minutes, 10_080);
        assert_eq!(snapshot.credits.unwrap().balance.as_deref(), Some("0"));
    }

    #[test]
    fn parses_rate_limit_status_detail_windows() {
        let value = serde_json::json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 42.5,
                    "limit_window_seconds": 18_000,
                    "reset_after_seconds": 600
                },
                "secondary_window": {
                    "remaining_percent": 72,
                    "limit_window_seconds": 604_800,
                    "reset_after_seconds": 86_400
                }
            }
        });

        let snapshot = parse_account_usage(&value, 1_700_000_000).unwrap();
        assert_eq!(snapshot.primary.unwrap().window_minutes, 300);
        let secondary = snapshot.secondary.unwrap();
        assert_eq!(secondary.used_percent, 28.0);
        assert_eq!(secondary.resets_at, Some(1_700_086_400));
    }

    #[test]
    fn parses_app_server_window_duration_fields() {
        let value = serde_json::json!({
            "rateLimits": {
                "primary": {
                    "usedPercent": 25,
                    "windowDurationMins": 300,
                    "resetsAt": 1_800_000_000
                },
                "secondary": {
                    "usedPercent": 50,
                    "windowDurationMins": 10_080,
                    "resetsAt": 1_800_500_000
                },
                "planType": "plus"
            }
        });

        let snapshot = parse_account_usage(&value, 1_700_000_000).unwrap();
        assert_eq!(snapshot.plan_type.as_deref(), Some("plus"));
        assert_eq!(snapshot.primary.unwrap().window_minutes, 300);
        assert_eq!(snapshot.secondary.unwrap().window_minutes, 10_080);
    }

    #[test]
    fn every_credential_file_keeps_its_own_usage_cache() {
        let mut caches = AccountUsageCaches::default();
        let first = std::path::PathBuf::from("/tmp/codey-usage/acct_1.json");
        let second = std::path::PathBuf::from("/tmp/codey-usage/acct_2.json");

        let first_cache = caches.for_auth_path(&first) as *const AccountUsageCache;
        assert!(
            std::ptr::eq(first_cache, caches.for_auth_path(&first)),
            "the same credential document reuses one cache entry"
        );
        assert!(
            !std::ptr::eq(first_cache, caches.for_auth_path(&second)),
            "a second account never reuses the first account cache entry"
        );

        for index in 0..MAX_ACCOUNT_USAGE_CACHES + 4 {
            let path = std::path::PathBuf::from(format!("/tmp/codey-usage/acct_{index}.json"));
            caches
                .for_auth_path(&path)
                .record_failure("offline".into(), Instant::now());
        }
        assert!(
            caches.caches.len() <= MAX_ACCOUNT_USAGE_CACHES,
            "the cache map stays bounded"
        );
    }

    #[test]
    fn usage_caches_evict_only_the_least_recently_used_credential_file() {
        let mut caches = AccountUsageCaches::default();
        let oldest = "/tmp/codey-usage/oldest.json";
        let recent = "/tmp/codey-usage/recent.json";
        caches
            .for_auth_path(Path::new(oldest))
            .record_failure("offline".into(), Instant::now());
        caches
            .for_auth_path(Path::new(recent))
            .record_failure("offline".into(), Instant::now());
        for index in 0..MAX_ACCOUNT_USAGE_CACHES - 2 {
            let path = format!("/tmp/codey-usage/fill_{index}.json");
            caches.for_auth_path(Path::new(&path));
        }
        assert_eq!(caches.caches.len(), MAX_ACCOUNT_USAGE_CACHES);
        // 再次读取 recent，让最早建立的 oldest 成为唯一的淘汰候选。
        caches.for_auth_path(Path::new(recent));
        caches.for_auth_path(Path::new("/tmp/codey-usage/overflow.json"));

        assert_eq!(caches.caches.len(), MAX_ACCOUNT_USAGE_CACHES);
        assert!(
            !caches.caches.contains_key(oldest),
            "超过上限时只淘汰最久未使用的凭据缓存"
        );
        assert!(caches.caches.contains_key(recent));
        assert!(caches.caches.contains_key("/tmp/codey-usage/overflow.json"));
    }

    #[test]
    fn auth_caches_evict_only_the_least_recently_used_credential_file() {
        let mut caches = OfficialAuthCaches::default();
        let oldest = "/tmp/codey-auth/oldest.json";
        let recent = "/tmp/codey-auth/recent.json";
        caches.for_path(Path::new(oldest));
        caches.for_path(Path::new(recent));
        for index in 0..MAX_ACCOUNT_USAGE_CACHES - 2 {
            let path = format!("/tmp/codey-auth/fill_{index}.json");
            caches.for_path(Path::new(&path));
        }
        caches.for_path(Path::new(recent));
        caches.for_path(Path::new("/tmp/codey-auth/overflow.json"));

        assert_eq!(caches.caches.len(), MAX_ACCOUNT_USAGE_CACHES);
        assert!(
            !caches.caches.contains_key(oldest),
            "超过上限时不清空整表，只淘汰最久未使用的凭据缓存"
        );
        assert!(caches.caches.contains_key(recent));
        assert!(caches.caches.contains_key("/tmp/codey-auth/overflow.json"));
    }
}
