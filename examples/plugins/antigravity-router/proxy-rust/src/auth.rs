// OAuth credentials: auth.json read/refresh, the linked-account store
// (antigravity-accounts.json), hard-quota failover, and the PKCE login flow.
// Port of upstream src/auth/{accounts,oauth}.ts + proxy server.mjs refresh.
use base64::Engine;
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

pub const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Default)]
pub struct Credential {
    pub access: String,
    pub refresh: String,
    pub expires_ms: i64,
    pub project_id: Option<String>,
    pub email: Option<String>,
}

impl Credential {
    pub fn access_fresh(&self) -> bool {
        !self.access.is_empty()
            && self.expires_ms > chrono::Utc::now().timestamp_millis() + REFRESH_SKEW_MS
    }
}

#[derive(Debug, Clone)]
pub struct StoredAccount {
    pub account_id: String,
    pub email: Option<String>,
    pub access: String,
    pub refresh: String,
    pub expires_ms: i64,
    pub project_id: Option<String>,
    pub added_at: i64,
    pub last_used_at: i64,
}

impl StoredAccount {
    pub fn access_fresh(&self) -> bool {
        !self.access.is_empty()
            && self.expires_ms > chrono::Utc::now().timestamp_millis() + REFRESH_SKEW_MS
    }
}

// --- Private JSON file I/O ----------------------------------------------------

pub fn read_json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

pub fn write_private_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        crate::convert::rand_u64()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        use std::io::Write;
        let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
        let data = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        file.write_all(&data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// --- auth.json ------------------------------------------------------------------

pub fn read_auth_credential(auth_path: &Path) -> Result<Credential, String> {
    let raw = read_json_file(auth_path).ok_or_else(|| "auth file unreadable".to_string())?;
    let cred = raw
        .get("antigravity")
        .filter(|c| c.is_object())
        .or_else(|| raw.get("provider").filter(|c| c.is_object()))
        .cloned()
        .ok_or_else(|| "no antigravity refresh token in auth file".to_string())?;
    let refresh = cred
        .get("refresh")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if refresh.is_empty() {
        return Err("no antigravity refresh token in auth file".into());
    }
    Ok(Credential {
        access: cred
            .get("access")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        refresh,
        expires_ms: cred.get("expires").and_then(|v| v.as_i64()).unwrap_or(0),
        project_id: cred
            .get("projectId")
            .and_then(|v| v.as_str())
            .map(String::from),
        email: cred.get("email").and_then(|v| v.as_str()).map(String::from),
    })
}

fn write_auth_credential(auth_path: &Path, account: Option<&StoredAccount>) -> Result<(), String> {
    let mut auth = read_json_file(auth_path).unwrap_or_else(|| json!({}));
    if !auth.is_object() {
        auth = json!({});
    }
    match account {
        Some(acc) => {
            auth["antigravity"] = json!({
                "type": "oauth",
                "email": acc.email,
                "access": acc.access,
                "refresh": acc.refresh,
                "expires": acc.expires_ms,
                "projectId": acc.project_id,
            });
        }
        None => {
            if let Some(obj) = auth.as_object_mut() {
                obj.remove("antigravity");
            }
        }
    }
    write_private_json(auth_path, &auth)
}

// --- Linked accounts store ------------------------------------------------------

fn load_account_store(accounts_path: &Path) -> Vec<StoredAccount> {
    let Some(raw) = read_json_file(accounts_path) else {
        return vec![];
    };
    let Some(accounts) = raw.get("accounts").and_then(|a| a.as_object()) else {
        return vec![];
    };
    let mut out: Vec<StoredAccount> = Vec::new();
    for (key, entry) in accounts {
        let refresh = entry.get("refresh").and_then(|v| v.as_str()).unwrap_or("");
        if refresh.is_empty() {
            continue;
        }
        let account_id = entry
            .get("accountId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(key)
            .to_string();
        out.push(StoredAccount {
            account_id,
            email: entry
                .get("email")
                .and_then(|v| v.as_str())
                .map(String::from),
            access: entry
                .get("access")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            refresh: refresh.to_string(),
            expires_ms: entry.get("expires").and_then(|v| v.as_i64()).unwrap_or(0),
            project_id: entry
                .get("projectId")
                .and_then(|v| v.as_str())
                .map(String::from),
            added_at: entry.get("addedAt").and_then(|v| v.as_i64()).unwrap_or(0),
            last_used_at: entry
                .get("lastUsedAt")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        });
    }
    out.sort_by(|a, b| {
        a.added_at
            .cmp(&b.added_at)
            .then(a.account_id.cmp(&b.account_id))
    });
    out
}

fn account_id_for(email: Option<&str>, refresh: &str) -> String {
    match email.map(str::trim).filter(|s| !s.is_empty()) {
        Some(e) => e.to_lowercase(),
        None => {
            let mut hasher = sha2::Sha256::new();
            hasher.update(refresh.as_bytes());
            let hex: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("account-{}", &hex[..16])
        }
    }
}

/// Upsert the current auth.json credential into the accounts store and mark
/// it active (port of syncCurrentAuth).
pub fn sync_current_auth(auth_path: &Path, accounts_path: &Path) {
    let Ok(cred) = read_auth_credential(auth_path) else {
        return;
    };
    let mut accounts = load_account_store(accounts_path);
    let account_id = account_id_for(cred.email.as_deref(), &cred.refresh);
    let now = chrono::Utc::now().timestamp_millis();
    let existing = accounts
        .iter()
        .position(|a| a.refresh == cred.refresh || a.account_id == account_id);
    let account_id = existing
        .map(|i| accounts[i].account_id.clone())
        .unwrap_or(account_id);
    let account = StoredAccount {
        account_id,
        email: cred.email.clone(),
        access: cred.access.clone(),
        refresh: cred.refresh.clone(),
        expires_ms: cred.expires_ms,
        project_id: cred.project_id.clone(),
        added_at: existing.map(|i| accounts[i].added_at).unwrap_or(now),
        last_used_at: now,
    };
    match existing {
        Some(i) => accounts[i] = account,
        None => accounts.push(account),
    }
    let active = account_id_for(cred.email.as_deref(), &cred.refresh);
    let active = accounts
        .iter()
        .find(|a| a.refresh == cred.refresh)
        .map(|a| a.account_id.as_str())
        .unwrap_or(&active);
    save_account_store(accounts_path, &accounts, Some(active));
}

fn save_account_store(accounts_path: &Path, accounts: &[StoredAccount], active: Option<&str>) {
    let mut map = serde_json::Map::new();
    for a in accounts {
        map.insert(
            a.account_id.clone(),
            json!({
                "accountId": a.account_id,
                "email": a.email,
                "access": a.access,
                "refresh": a.refresh,
                "expires": a.expires_ms,
                "projectId": a.project_id,
                "addedAt": a.added_at,
                "lastUsedAt": a.last_used_at,
            }),
        );
    }
    let store = json!({
        "version": 1,
        "activeAccountId": active,
        "accounts": Value::Object(map)
    });
    let _ = write_private_json(accounts_path, &store);
}

pub fn list_accounts(accounts_path: &Path) -> (Vec<StoredAccount>, Option<String>) {
    let accounts = load_account_store(accounts_path);
    let active = read_json_file(accounts_path).and_then(|raw| {
        raw.get("activeAccountId")
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    (accounts, active)
}

pub fn find_account_id(accounts: &[StoredAccount], selector: &str) -> Option<String> {
    let sel = selector.trim().to_lowercase();
    if sel.is_empty() {
        return None;
    }
    if let Ok(index) = sel.parse::<usize>() {
        if (1..=accounts.len()).contains(&index) {
            return accounts.get(index - 1).map(|a| a.account_id.clone());
        }
        return None;
    }
    accounts
        .iter()
        .find(|a| {
            a.account_id.to_lowercase() == sel
                || a.email
                    .as_deref()
                    .map(|e| e.trim().to_lowercase())
                    .as_deref()
                    == Some(sel.as_str())
        })
        .map(|a| a.account_id.clone())
}

/// Switch the active account (refreshing it if stale) and rewrite auth.json.
pub async fn activate_account(
    auth_path: &Path,
    accounts_path: &Path,
    selector: &str,
) -> Result<StoredAccount, String> {
    sync_current_auth(auth_path, accounts_path);
    let mut accounts = load_account_store(accounts_path);
    let account_id = find_account_id(&accounts, selector)
        .ok_or_else(|| format!("Antigravity account not found: {selector}"))?;
    let idx = accounts
        .iter()
        .position(|a| a.account_id == account_id)
        .unwrap();
    let mut account = refresh_account_if_stale(accounts[idx].clone()).await;
    account.last_used_at = chrono::Utc::now().timestamp_millis();
    accounts[idx] = account.clone();
    save_account_store(accounts_path, &accounts, Some(&account.account_id));
    write_auth_credential(auth_path, Some(&account))?;
    Ok(account)
}

pub async fn remove_account(
    auth_path: &Path,
    accounts_path: &Path,
    selector: &str,
) -> Result<Option<StoredAccount>, String> {
    sync_current_auth(auth_path, accounts_path);
    let mut accounts = load_account_store(accounts_path);
    let account_id = find_account_id(&accounts, selector)
        .ok_or_else(|| format!("Antigravity account not found: {selector}"))?;
    let active = read_json_file(accounts_path).and_then(|raw| {
        raw.get("activeAccountId")
            .and_then(|v| v.as_str())
            .map(String::from)
    });
    let was_active = active.as_deref() == Some(account_id.as_str());
    accounts.retain(|a| a.account_id != account_id);
    let mut next: Option<StoredAccount> = None;
    if was_active {
        next = accounts.iter().cloned().max_by_key(|a| a.last_used_at);
        if let Some(n) = next.clone() {
            let refreshed = refresh_account_if_stale(n).await;
            if let Some(slot) = accounts
                .iter_mut()
                .find(|a| a.account_id == refreshed.account_id)
            {
                *slot = refreshed.clone();
            }
            next = Some(refreshed);
        }
        write_auth_credential(auth_path, next.as_ref())?;
    }
    save_account_store(
        accounts_path,
        &accounts,
        next.as_ref()
            .map(|n| n.account_id.as_str())
            .or(active.as_deref()),
    );
    Ok(next)
}

// --- Token refresh -----------------------------------------------------------------

fn token_cache() -> &'static Mutex<HashSet<String>> {
    static CACHE: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn inflight_refreshes() -> &'static Mutex<HashSet<String>> {
    static SET: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

pub async fn refresh_access_token(
    refresh_token: &str,
) -> Result<(String, i64, Option<String>), String> {
    // De-dupe concurrent refreshes for the same refresh token.
    loop {
        let inflight = {
            let mut set = inflight_refreshes()
                .lock()
                .map_err(|_| "lock poisoned".to_string())?;
            if set.contains(refresh_token) {
                true
            } else {
                set.insert(refresh_token.to_string());
                false
            }
        };
        if inflight {
            tokio::time::sleep(Duration::from_millis(150)).await;
            continue;
        }
        break;
    }
    let result = refresh_access_token_uncached(refresh_token).await;
    if let Ok(mut set) = inflight_refreshes().lock() {
        set.remove(refresh_token);
    }
    result
}

async fn refresh_access_token_uncached(
    refresh_token: &str,
) -> Result<(String, i64, Option<String>), String> {
    let (client_id, client_secret) = crate::config::oauth_client_credentials()?;
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token.to_string()),
        ("grant_type", "refresh_token".to_string()),
    ];
    let client = reqwest::Client::new();
    let resp = client
        .post(crate::config::TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&params)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("token refresh failed: {}", crate::security::safe_error(e)))?;
    let status = resp.status();
    let text = crate::upstream::read_text_capped(resp, 65536, 30).await?;
    if !status.is_success() {
        return Err(format!(
            "token refresh failed HTTP {}: {}",
            status.as_u16(),
            crate::security::redact_secrets(&crate::client_util::json_or_text_error(&text))
                .chars()
                .take(300)
                .collect::<String>()
        ));
    }
    let data: Value =
        serde_json::from_str(&text).map_err(|e| format!("token refresh parse: {e}"))?;
    let access = data
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("token refresh response missing access_token")?
        .to_string();
    let expires_in = data
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let rotated = data
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(String::from);
    let expiry = chrono::Utc::now().timestamp_millis() + expires_in * 1000 - REFRESH_SKEW_MS;
    if let Ok(mut cache) = token_cache().lock() {
        cache.insert(access.clone());
    }
    Ok((access, expiry, rotated))
}

async fn refresh_account_if_stale(mut account: StoredAccount) -> StoredAccount {
    if account.access_fresh() {
        return account;
    }
    match refresh_access_token(&account.refresh).await {
        Ok((access, expires, rotated)) => {
            account.access = access;
            account.expires_ms = expires;
            if let Some(r) = rotated {
                account.refresh = r;
            }
            account
        }
        Err(_) => account,
    }
}

/// The proxy-side credential resolution: read auth.json, return a fresh access
/// token (memory cache first, then refresh + persist).
pub async fn fresh_credential(
    auth_path: &Path,
    accounts_path: &Path,
) -> Result<(Credential, String), String> {
    let _guard = credential_update_lock().lock().await;
    let cred = read_auth_credential(auth_path)?;
    if cred.access_fresh() {
        return Ok((cred.clone(), cred.access.clone()));
    }
    let (access, expires, rotated) = refresh_access_token(&cred.refresh).await?;
    let mut updated = cred.clone();
    updated.access = access.clone();
    updated.expires_ms = expires;
    if let Some(r) = rotated {
        updated.refresh = r;
    }
    persist_refreshed(auth_path, accounts_path, &updated);
    Ok((updated, access))
}

fn persist_refreshed(auth_path: &Path, accounts_path: &Path, cred: &Credential) {
    // Best-effort persistence of the refreshed token (auth.json + account store).
    let previous = read_auth_credential(auth_path).ok();
    let mut accounts = load_account_store(accounts_path);
    if let Some(previous) = previous {
        if let Some(account) = accounts.iter_mut().find(|a| a.refresh == previous.refresh) {
            account.access = cred.access.clone();
            account.refresh = cred.refresh.clone();
            account.expires_ms = cred.expires_ms;
            let active = account.account_id.clone();
            save_account_store(accounts_path, &accounts, Some(&active));
        }
    }
    if let Some(mut auth) = read_json_file(auth_path) {
        if let Some(obj) = auth.as_object_mut() {
            if let Some(entry) = obj.get_mut("antigravity").filter(|e| e.is_object()) {
                entry["access"] = json!(cred.access);
                entry["expires"] = json!(cred.expires_ms);
                entry["refresh"] = json!(cred.refresh);
            }
        }
        let _ = write_private_json(auth_path, &auth);
    }
    sync_current_auth(auth_path, accounts_path);
}

// --- Hard-quota failover --------------------------------------------------------------

/// Switch to the next linked account whose access token was not already tried.
/// Updates the account store + auth.json (port of failoverToNextAccount).
pub async fn failover_to_next_account(
    auth_path: &Path,
    accounts_path: &Path,
    tried_access_tokens: &HashSet<String>,
) -> Option<(Credential, String)> {
    let _guard = credential_update_lock().lock().await;
    sync_current_auth(auth_path, accounts_path);
    let mut accounts = load_account_store(accounts_path);
    for index in 0..accounts.len() {
        let candidate = accounts[index].clone();
        if !candidate.access.is_empty() && tried_access_tokens.contains(&candidate.access) {
            continue;
        }
        let refreshed = refresh_account_if_stale(candidate).await;
        if !refreshed.access_fresh() || tried_access_tokens.contains(&refreshed.access) {
            continue;
        }
        let mut updated = refreshed.clone();
        updated.last_used_at = chrono::Utc::now().timestamp_millis();
        if let Some(slot) = accounts
            .iter_mut()
            .find(|a| a.account_id == updated.account_id)
        {
            *slot = updated.clone();
        }
        save_account_store(accounts_path, &accounts, Some(&updated.account_id));
        if write_auth_credential(auth_path, Some(&updated)).is_err() {
            continue;
        }
        let cred = Credential {
            access: updated.access.clone(),
            refresh: updated.refresh.clone(),
            expires_ms: updated.expires_ms,
            project_id: updated.project_id.clone(),
            email: updated.email.clone(),
        };
        return Some((cred, updated.access));
    }
    None
}

fn credential_update_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod failover_tests {
    use super::*;
    #[test]
    fn refresh_rotation_preserves_identity_without_email() {
        let dir =
            std::env::temp_dir().join(format!("ag-rotation-test-{}", crate::convert::rand_u64()));
        std::fs::create_dir(&dir).unwrap();
        let auth = dir.join("auth.json");
        let store = dir.join("accounts.json");
        let old = StoredAccount {
            account_id: account_id_for(None, "old-test-refresh"),
            email: None,
            access: "old-access".into(),
            refresh: "old-test-refresh".into(),
            expires_ms: 0,
            project_id: Some("test-project".into()),
            added_at: 1,
            last_used_at: 1,
        };
        save_account_store(&store, &[old.clone()], Some(&old.account_id));
        write_auth_credential(&auth, Some(&old)).unwrap();
        let updated = Credential {
            access: "new-test-access".into(),
            refresh: "new-test-refresh".into(),
            expires_ms: i64::MAX / 2,
            project_id: old.project_id.clone(),
            email: None,
        };
        persist_refreshed(&auth, &store, &updated);
        sync_current_auth(&auth, &store);
        let accounts = load_account_store(&store);
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].account_id, old.account_id);
        assert_eq!(accounts[0].refresh, updated.refresh);
        assert_eq!(
            read_json_file(&store).unwrap()["activeAccountId"],
            old.account_id
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn callback_rejects_wrong_origin_and_handles_unicode_errors() {
        assert!(
            parse_pasted_callback("http://localhost:51121/oauth-callback?code=c&state=s", "s")
                .is_ok()
        );
        assert!(parse_pasted_callback("?code=c&state=s", "s").is_ok());
        for url in [
            "https://evil.test/?code=c&state=s",
            "http://localhost:51121/wrong?code=c&state=s",
            "http://user@localhost:51121/oauth-callback?code=c&state=s",
        ] {
            assert!(parse_pasted_callback(url, "s").is_err());
        }
        let err = format!("?error={}", urlencoding::encode(&"中".repeat(100)));
        assert!(parse_pasted_callback(&err, "s").is_err());
    }
    #[tokio::test]
    async fn failover_skips_tried_tokens_and_updates_project_and_store() {
        let dir =
            std::env::temp_dir().join(format!("ag-failover-test-{}", crate::convert::rand_u64()));
        std::fs::create_dir(&dir).unwrap();
        let auth = dir.join("auth.json");
        let store = dir.join("accounts.json");
        let make = |id: &str, token: &str, project: &str| StoredAccount {
            account_id: id.into(),
            email: Some(format!("{id}@example.test")),
            access: token.into(),
            refresh: "synthetic-refresh".into(),
            expires_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
            project_id: Some(project.into()),
            added_at: 0,
            last_used_at: 0,
        };
        let a = make("a", "synthetic-a", "project-a");
        let b = make("b", "synthetic-b", "project-b");
        save_account_store(&store, &[a.clone(), b], Some("a"));
        write_auth_credential(&auth, Some(&a)).unwrap();
        let mut tried = HashSet::from(["synthetic-a".into()]);
        let (cred, token) = failover_to_next_account(&auth, &store, &tried)
            .await
            .unwrap();
        assert_eq!(token, "synthetic-b");
        assert_eq!(cred.project_id.as_deref(), Some("project-b"));
        assert_eq!(read_auth_credential(&auth).unwrap().access, "synthetic-b");
        assert_eq!(read_json_file(&store).unwrap()["activeAccountId"], "b");
        tried.insert(token);
        assert!(failover_to_next_account(&auth, &store, &tried)
            .await
            .is_none());
        std::fs::remove_file(auth).unwrap();
        std::fs::remove_file(store).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}

// --- PKCE login (CLI `login` subcommand) -----------------------------------------------

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_pkce() -> (String, String) {
    use rand::RngCore;
    let mut verifier_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = base64url(&verifier_bytes);
    let mut hasher = sha2::Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64url(&hasher.finalize());
    (verifier, challenge)
}

pub struct LoginOutcome {
    pub email: Option<String>,
    pub project_id: Option<String>,
}

/// Full PKCE login: local callback listener on 127.0.0.1:51121 (or a pasted
/// callback URL for headless machines), token exchange, account registration.
pub async fn login(
    auth_path: &Path,
    accounts_path: &Path,
    manual: bool,
) -> Result<LoginOutcome, String> {
    let (client_id, _) = crate::config::oauth_client_credentials()?;
    let (verifier, challenge) = generate_pkce();
    // State is independent of the PKCE verifier so a leaked callback URL
    // cannot also disclose the code_verifier.
    use rand::RngCore;
    let mut state_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    let state = base64url(&state_bytes);

    let auth_params = [
        ("client_id", client_id),
        ("response_type", "code".to_string()),
        ("redirect_uri", crate::config::REDIRECT_URI.to_string()),
        ("scope", crate::config::SCOPES.to_string()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".to_string()),
        ("state", state.clone()),
        ("access_type", "offline".to_string()),
        ("prompt", "consent".to_string()),
    ];
    let query: Vec<String> = auth_params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect();
    println!(
        "\nOpen this URL in a browser to sign in:\n\n{}?{}\n",
        crate::config::AUTH_URL,
        query.join("&")
    );
    let (code, _returned_state) = if manual {
        println!("Paste the full callback URL below while this login process is still running:");
        let mut raw = String::new();
        std::io::stdin()
            .read_line(&mut raw)
            .map_err(|_| "Unable to read callback from stdin")?;
        parse_pasted_callback(&raw, &state)?
    } else {
        println!("(For a machine without a local callback listener, use `login --manual`.)");
        wait_for_callback(&state).await?
    };
    login_exchange(auth_path, accounts_path, &code, &verifier).await
}

fn parse_pasted_callback(raw: &str, expected_state: &str) -> Result<(String, String), String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("No callback pasted. Paste the full URL from your browser's address bar (http://localhost:51121/oauth-callback?…).".into());
    }
    let parsed = match url::Url::parse(text) {
        Ok(u) => u,
        Err(_) => {
            let qs = text.trim_start_matches('?');
            url::Url::parse(&format!("http://localhost:51121/oauth-callback?{qs}"))
                .map_err(|e| e.to_string())?
        }
    };
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("localhost")
        || parsed.port() != Some(crate::config::OAUTH_CALLBACK_PORT)
        || parsed.path() != "/oauth-callback"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err("Expected the localhost OAuth callback URL or its query string".into());
    }
    if let Some(err) = parsed
        .query_pairs()
        .find(|(k, _)| k == "error")
        .map(|(_, v)| v.to_string())
    {
        return Err(format!(
            "OAuth error from browser: {}",
            crate::security::redact_secrets(&err)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    let code = parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string());
    let state = parsed
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());
    match (code, state) {
        (Some(c), Some(s)) if s == expected_state => Ok((c, s)),
        (Some(_), Some(_)) => Err("State mismatch: that callback is from a different sign-in. Re-run login and paste the new URL.".into()),
        _ => Err("Pasted text is missing 'code' or 'state'. Paste the FULL callback URL.".into()),
    }
}

async fn wait_for_callback(expected_state: &str) -> Result<(String, String), String> {
    let host = crate::security::resolve_callback_host();
    let addr = format!("{host}:{}", crate::config::OAUTH_CALLBACK_PORT);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            format!(
                "Port {} is already in use. Close the process using it and retry login.",
                crate::config::OAUTH_CALLBACK_PORT
            )
        } else {
            e.to_string()
        }
    })?;
    println!(
        "[login] waiting for the Google OAuth callback on http://localhost:{}/oauth-callback ...",
        crate::config::OAUTH_CALLBACK_PORT
    );
    let timeout = tokio::time::timeout(
        Duration::from_millis(crate::config::OAUTH_CALLBACK_TIMEOUT_MS),
        async {
            loop {
                let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
                match handle_callback_conn(stream, expected_state).await {
                    Ok(Some(result)) => return Ok::<(String, String), String>(result),
                    Ok(None) => continue,
                    Err(e) => return Err(e),
                }
            }
        },
    )
    .await
    .map_err(|_| "OAuth callback timed out waiting for browser login".to_string())??;
    Ok(timeout)
}

async fn handle_callback_conn(
    mut stream: tokio::net::TcpStream,
    expected_state: &str,
) -> Result<Option<(String, String)>, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let request_line = request.lines().next().unwrap_or("");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let is_head = request_line.starts_with("HEAD");
    if !request_line.starts_with("GET") && !is_head {
        let _ = stream
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Type: text/plain\r\nContent-Length: 17\r\n\r\nMethod Not Allowed")
            .await;
        return Ok(None);
    }
    let parsed = url::Url::parse(&format!("http://localhost{path}")).map_err(|e| e.to_string())?;
    if parsed.path() != "/oauth-callback" {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 42\r\n\r\nAntigravity OAuth callback route not found.\r\n")
            .await;
        return Ok(None);
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.to_string()),
            "state" => state = Some(v.to_string()),
            "error" => error = Some(v.to_string()),
            _ => {}
        }
    }
    if let Some(err) = error {
        let body_text = format!(
            "Antigravity authentication failed: {}",
            crate::security::redact_secrets(&err)
        );
        let _ = stream
            .write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\n\r\n{}", body_text.len(), body_text).as_bytes())
            .await;
        return Err(format!(
            "OAuth error: {}",
            crate::security::redact_secrets(&err)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }
    match (code, state) {
        (Some(c), Some(s)) if s == expected_state => {
            let body = "Antigravity authentication complete. You can close this window and return to the CLI.";
            let _ = stream
                .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).as_bytes())
                .await;
            Ok(Some((c, s)))
        }
        (Some(_), Some(_)) => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nContent-Length: 37\r\n\r\nAntigravity authentication failed: invalid state.\r\n").await;
            Err("OAuth state mismatch".into())
        }
        _ => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nContent-Length: 44\r\n\r\nAntigravity authentication failed: missing code or state.\r\n").await;
            Err("Missing code or state in OAuth callback".into())
        }
    }
}

/// Complete the login after the auth code is acquired: exchange, fetch email +
/// project, persist to auth.json + accounts.json.
pub async fn login_exchange(
    auth_path: &Path,
    accounts_path: &Path,
    code: &str,
    verifier: &str,
) -> Result<LoginOutcome, String> {
    let (client_id, client_secret) = crate::config::oauth_client_credentials()?;
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code", code.to_string()),
        ("grant_type", "authorization_code".to_string()),
        ("redirect_uri", crate::config::REDIRECT_URI.to_string()),
        ("code_verifier", verifier.to_string()),
    ];
    let client = reqwest::Client::new();
    let resp = client
        .post(crate::config::TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&params)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("token exchange failed: {}", crate::security::safe_error(e)))?;
    let status = resp.status();
    let text = crate::upstream::read_text_capped(resp, 65536, 30).await?;
    if !status.is_success() {
        return Err(format!(
            "Token exchange failed: {}",
            sanitize_oauth_error(&text)
        ));
    }
    let data: Value =
        serde_json::from_str(&text).map_err(|e| format!("token exchange parse: {e}"))?;
    let access = data
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let refresh_token = data
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if refresh_token.is_empty() {
        return Err("No refresh token received. Re-run login and allow offline access.".into());
    }
    let expires_in = data
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let email = get_user_email(&access).await;
    let project_id = match crate::discovery::load_code_assist(&access).await {
        Some(p) => Some(p),
        None => None,
    };
    let now = chrono::Utc::now().timestamp_millis();
    let account_id = account_id_for(email.as_deref(), &refresh_token);
    let account = StoredAccount {
        account_id: account_id.clone(),
        email: email.clone(),
        access: access.clone(),
        refresh: refresh_token.clone(),
        expires_ms: now + expires_in * 1000 - REFRESH_SKEW_MS,
        project_id: project_id.clone(),
        added_at: now,
        last_used_at: now,
    };
    // Preserve addedAt for an existing account with the same id.
    let mut accounts = load_account_store(accounts_path);
    let account = match accounts.iter().position(|a| a.account_id == account_id) {
        Some(i) => {
            let added = accounts[i].added_at;
            accounts[i] = StoredAccount {
                added_at: added,
                ..account
            };
            accounts[i].clone()
        }
        None => {
            accounts.push(account.clone());
            account.clone()
        }
    };
    save_account_store(accounts_path, &accounts, Some(&account_id));
    write_auth_credential(auth_path, Some(&account))?;
    Ok(LoginOutcome { email, project_id })
}

async fn get_user_email(access: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://www.googleapis.com/oauth2/v1/userinfo?alt=json")
        .header("Authorization", format!("Bearer {access}"))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: Value = resp.json().await.ok()?;
    data.get("email").and_then(|v| v.as_str()).map(String::from)
}

fn sanitize_oauth_error(text: &str) -> String {
    let redacted = crate::security::redact_secrets(text).trim().to_string();
    if let Ok(parsed) = serde_json::from_str::<Value>(&redacted) {
        let parts: Vec<String> = ["error", "error_description"]
            .iter()
            .filter_map(|k| parsed.get(k).and_then(|v| v.as_str()).map(String::from))
            .collect();
        if !parts.is_empty() {
            return parts.join(": ").chars().take(300).collect();
        }
    }
    redacted.chars().take(300).collect()
}
