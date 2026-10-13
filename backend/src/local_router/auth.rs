use super::*;

pub(crate) struct OfficialUpstreamAuth {
    pub(crate) authorization: String,
    pub(crate) account_id: Option<String>,
}

struct IncomingTokenIatCache {
    token: String,
    issued_at: Option<u64>,
}

/// Codex 在同一登录期内会反复发送同一条请求头令牌；只保留最近一次解码结果。
static LAST_INCOMING_TOKEN_IAT: Mutex<Option<IncomingTokenIatCache>> = Mutex::new(None);

pub(crate) fn incoming_chatgpt_account_id(request: &HttpRequest) -> Option<String> {
    incoming_header(request, CHATGPT_ACCOUNT_ID_HEADER)
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty())
        .map(ToString::to_string)
}

/// 取出 Bearer 方案携带的令牌，其他方案或空值返回 None。
fn bearer_token(authorization: &str) -> Option<&str> {
    let (scheme, token) = authorization.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

fn access_token_issued_at(token: &str) -> Option<u64> {
    crate::official_accounts::access_token_issued_at(token)
}

fn incoming_access_token_issued_at(token: &str) -> Option<u64> {
    let mut cache = LAST_INCOMING_TOKEN_IAT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = cache.as_ref()
        && cached.token == token
    {
        return cached.issued_at;
    }
    let issued_at = access_token_issued_at(token);
    *cache = Some(IncomingTokenIatCache {
        token: token.to_string(),
        issued_at,
    });
    issued_at
}

/// 已启动的 Codex 进程会把登录时拿到的令牌留在内存里；用户在 Codey 里重新
/// 登录后账号文件已经换成新令牌，请求头仍会带着旧令牌。两份令牌都能读出签发
/// 时间时取较新的一份；请求头不是可解读的 JWT 而账号文件有令牌时也以账号
/// 文件为准，官方端点只接受这种登录令牌，避免把已经失效的旧令牌继续转发。
#[cfg(test)]
pub(crate) fn stored_token_supersedes_incoming(
    incoming_authorization: &str,
    stored_access_token: &str,
) -> bool {
    stored_token_supersedes_incoming_issued_at(
        incoming_authorization,
        stored_access_token,
        access_token_issued_at(stored_access_token),
    )
}

fn stored_auth_supersedes_incoming(
    incoming_authorization: &str,
    stored: &crate::account_usage::OfficialAuth,
) -> bool {
    stored_token_supersedes_incoming_issued_at(
        incoming_authorization,
        &stored.access_token,
        stored.issued_at,
    )
}

fn stored_token_supersedes_incoming_issued_at(
    incoming_authorization: &str,
    stored_access_token: &str,
    stored_issued_at: Option<u64>,
) -> bool {
    let incoming_token = bearer_token(incoming_authorization);
    if incoming_token == Some(stored_access_token) {
        return false;
    }
    let incoming_issued_at = incoming_token.and_then(incoming_access_token_issued_at);
    match (incoming_issued_at, stored_issued_at) {
        (Some(incoming), Some(stored)) => stored > incoming,
        (None, Some(_)) => true,
        _ => false,
    }
}

pub(crate) async fn resolve_official_upstream_auth(
    request: &HttpRequest,
    router_bearer_token: &str,
    auth_path: &Path,
    auth_caches: &Mutex<crate::account_usage::OfficialAuthCaches>,
    // Only the account Codex itself is logged in as may reuse the token the
    // downstream request carries. Every other account reads its own document,
    // so one conversation never spends another account's login.
    accepts_incoming_authorization: bool,
) -> Option<OfficialUpstreamAuth> {
    if accepts_incoming_authorization
        && let Some(authorization) = incoming_openai_authorization(request, router_bearer_token)
    {
        // 重新登录只替换账号文件，已经启动的 Codex 仍会发送登录时缓存的旧
        // 令牌；账号文件里的令牌更新时以它为准，新登录才能立即生效。
        let stored = read_cached_official_auth(auth_path, auth_caches).await;
        if let Some(stored_auth) = stored.as_ref()
            && stored_auth_supersedes_incoming(authorization, stored_auth)
        {
            return Some(OfficialUpstreamAuth {
                authorization: format!("Bearer {}", stored_auth.access_token),
                account_id: stored_auth
                    .account_id
                    .clone()
                    .or_else(|| incoming_chatgpt_account_id(request)),
            });
        }
        let account_id = match incoming_chatgpt_account_id(request) {
            Some(account_id) => Some(account_id),
            None => stored.and_then(|auth| auth.account_id.clone()),
        };
        return Some(OfficialUpstreamAuth {
            authorization: authorization.to_string(),
            account_id,
        });
    }

    let auth = read_cached_official_auth(auth_path, auth_caches).await?;
    Some(OfficialUpstreamAuth {
        authorization: format!("Bearer {}", auth.access_token),
        // The account ID stored with the selected OAuth token is authoritative.
        // An incoming value may have been captured by a long-lived downstream
        // WebSocket before the user switched accounts.
        account_id: auth
            .account_id
            .clone()
            .or_else(|| incoming_chatgpt_account_id(request)),
    })
}

pub(crate) async fn read_cached_official_auth(
    auth_path: &Path,
    auth_caches: &Mutex<crate::account_usage::OfficialAuthCaches>,
) -> Option<std::sync::Arc<crate::account_usage::OfficialAuth>> {
    let now = Instant::now();
    if let Some(cached) = auth_caches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .for_path(auth_path)
        .get(auth_path, now)
    {
        return cached.ok();
    }

    let read_path = auth_path.to_path_buf();
    let result =
        tokio::task::spawn_blocking(move || crate::account_usage::read_official_auth(&read_path))
            .await
            .ok()?;
    let now = Instant::now();
    let mut caches = auth_caches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cache = caches.for_path(auth_path);
    if let Some(cached) = cache.get(auth_path, now) {
        return cached.ok();
    }
    cache.store(auth_path, result, now).ok()
}

// ---------------------------------------------------------------------------
// SIWC（官方直连）凭据解析
// ---------------------------------------------------------------------------

/// SIWC 凭据文件的读取缓存；与 `OfficialAuthCaches` 相同的
/// 指纹+短 TTL 策略，但缓存的是直连凭据视图。
#[derive(Debug)]
pub(crate) struct SiwcAuthCache {
    cached: Option<std::result::Result<Arc<crate::official_accounts::SiwcFileCredential>, String>>,
    fingerprint: Option<crate::account_usage::OfficialAuthFingerprint>,
    expires_at: Option<Instant>,
    last_used: u64,
}

/// 多账号 SIWC 文件缓存；键是文件路径，一个账号的凭据不会答到另一个账号。
#[derive(Debug, Default)]
pub(crate) struct SiwcAuthCaches {
    caches: HashMap<String, SiwcAuthCache>,
    usage_clock: u64,
}

impl SiwcAuthCaches {
    fn for_path(&mut self, path: &Path) -> &mut SiwcAuthCache {
        let key = path.to_string_lossy().into_owned();
        if !self.caches.contains_key(&key)
            && self.caches.len() >= 24
            && let Some(oldest) = self
                .caches
                .iter()
                .min_by_key(|(_, cache)| cache.last_used)
                .map(|(key, _)| key.clone())
        {
            self.caches.remove(&oldest);
        }
        self.usage_clock = self.usage_clock.saturating_add(1);
        let last_used = self.usage_clock;
        let cache = self.caches.entry(key).or_insert_with(|| SiwcAuthCache {
            cached: None,
            fingerprint: None,
            expires_at: None,
            last_used,
        });
        cache.last_used = last_used;
        cache
    }
}

impl SiwcAuthCache {
    fn get(
        &mut self,
        path: &Path,
        now: Instant,
    ) -> Option<std::result::Result<Arc<crate::official_accounts::SiwcFileCredential>, String>>
    {
        const REVALIDATE: Duration = Duration::from_secs(1);
        if self.expires_at.is_some_and(|expires_at| now < expires_at) {
            return self.cached.clone();
        }
        if self.cached.is_none() || self.fingerprint.is_none() {
            return None;
        }
        if crate::account_usage::official_auth_fingerprint(path) != self.fingerprint {
            return None;
        }
        self.expires_at = Some(now + REVALIDATE);
        self.cached.clone()
    }

    fn store(
        &mut self,
        path: &Path,
        result: Result<crate::official_accounts::SiwcFileCredential>,
        now: Instant,
    ) -> Result<Arc<crate::official_accounts::SiwcFileCredential>> {
        const REVALIDATE: Duration = Duration::from_secs(1);
        let result = result.map(Arc::new).map_err(|error| error.to_string());
        self.fingerprint = crate::account_usage::official_auth_fingerprint(path);
        self.cached = Some(result.clone());
        self.expires_at = Some(now + REVALIDATE);
        result.map_err(anyhow::Error::msg)
    }
}

async fn read_cached_siwc_auth(
    path: &Path,
    caches: &Mutex<SiwcAuthCaches>,
) -> Result<Arc<crate::official_accounts::SiwcFileCredential>> {
    let now = Instant::now();
    if let Some(result) = caches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .for_path(path)
        .get(path, now)
    {
        return result.map_err(anyhow::Error::msg);
    }
    let read_path = path.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        crate::official_accounts::read_siwc_credential_file(&read_path)
            .and_then(|credential| credential.context("官方直连凭据文件缺失"))
    })
    .await
    .context("读取官方直连凭据任务异常退出")?;
    let now = Instant::now();
    let mut caches = caches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cache = caches.for_path(path);
    if let Some(cached) = cache.get(path, now) {
        return cached.map_err(anyhow::Error::msg);
    }
    cache.store(path, result, now)
}

/// 解析 SIWC 线路的上游凭据：读物化文件，临期时走串行刷新，只返回
/// Authorization——公开 API 不需要也不会接受 `chatgpt-account-id`。
///
/// 返回值与 `resolve_official_upstream_auth` 共用同一类型；
/// `account_id` 恒为 None，调用方不再注入账号头。
pub(crate) async fn resolve_siwc_upstream_auth(
    siwc_path: &Path,
    account_id: &str,
    caches: &Mutex<SiwcAuthCaches>,
    default_client: &reqwest::Client,
    proxied_clients: &Mutex<HashMap<String, reqwest::Client>>,
) -> Result<OfficialUpstreamAuth> {
    let credential = read_cached_siwc_auth(siwc_path, caches).await?;
    if let Some(reason) = credential.invalid_reason.as_deref() {
        anyhow::bail!("官方直连授权已失效（{reason}），请在账号卡片重新授权");
    }
    if credential.needs_refresh(
        crate::official_accounts::unix_timestamp(),
        crate::official_accounts::SIWC_REFRESH_MARGIN_SECONDS,
    ) {
        let store = crate::official_accounts::OfficialAccountStore::for_config_path(
            &crate::config::default_config_path(),
        );
        if let Err(error) = crate::official_accounts::refresh_siwc_if_stale(
            default_client,
            &store,
            account_id,
            Some(proxied_clients),
        )
        .await
        {
            // 刷新失败但磁盘令牌仍未过期时继续用它：一次网络抖动不该打断
            // 还能用的对话；令牌真过期了才报错。
            let current = read_cached_siwc_auth(siwc_path, caches).await?;
            let now = crate::official_accounts::unix_timestamp();
            if current.expires_at != 0
                && current.expires_at > now
                && current.invalid_reason.is_none()
            {
                return Ok(OfficialUpstreamAuth {
                    authorization: format!("Bearer {}", current.access_token),
                    account_id: None,
                });
            }
            return Err(error);
        }
        let credential = read_cached_siwc_auth(siwc_path, caches).await?;
        if let Some(reason) = credential.invalid_reason.as_deref() {
            anyhow::bail!("官方直连授权已失效（{reason}），请在账号卡片重新授权");
        }
        return Ok(OfficialUpstreamAuth {
            authorization: format!("Bearer {}", credential.access_token),
            account_id: None,
        });
    }
    Ok(OfficialUpstreamAuth {
        authorization: format!("Bearer {}", credential.access_token),
        account_id: None,
    })
}

pub(crate) fn route_display_name(route: &RouteTarget) -> &str {
    let route_name = route.route_name.trim();
    if route_name.is_empty() {
        route.provider_id.as_str()
    } else {
        route_name
    }
}

pub(crate) fn upstream_authority(base_url: &str) -> String {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return "已配置地址".to_string();
    };
    let Some(host) = url.host_str() else {
        return "已配置地址".to_string();
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}
