// Security helpers: secret redaction, safe base URL, stable UUIDs, email masking.
use sha1::Digest;
use std::sync::OnceLock;

static REDACT_ACCESS: OnceLock<regex::Regex> = OnceLock::new();
static REDACT_REFRESH: OnceLock<regex::Regex> = OnceLock::new();
static REDACT_BEARER: OnceLock<regex::Regex> = OnceLock::new();
static REDACT_JSON_QUOTED: OnceLock<regex::Regex> = OnceLock::new();
static REDACT_JSON_BARE: OnceLock<regex::Regex> = OnceLock::new();

fn re(cell: &'static OnceLock<regex::Regex>, pattern: &str) -> &'static regex::Regex {
    cell.get_or_init(|| regex::Regex::new(pattern).expect("valid redaction regex"))
}

/// Redact bearer tokens, refresh tokens, and JSON key/value secrets from
/// diagnostics and error text (port of upstream utils/security.ts).
pub fn redact_secrets(text: &str) -> String {
    let t = re(&REDACT_ACCESS, r"\bya29\.[A-Za-z0-9._~+/-]+=*")
        .replace_all(text, "[redacted-access-token]")
        .into_owned();
    let t = re(&REDACT_REFRESH, r"\b1/+[A-Za-z0-9._~-]{20,}")
        .replace_all(&t, "[redacted-refresh-token]")
        .into_owned();
    let t = re(&REDACT_BEARER, r"(?i)\bBearer\s+[A-Za-z0-9._~+/-]+=*")
        .replace_all(&t, "Bearer [redacted]")
        .into_owned();
    let t = re(
        &REDACT_JSON_QUOTED,
        r#"(?i)"?(?:access_token|refresh_token|id_token|token|client_secret|code_verifier|authorization)"?\s*[:=]\s*"(?:\\.|[^"\\])*""#,
    )
    .replace_all(&t, |caps: &regex::Captures| {
        // Keep the key + separator, redact only the quoted value.
        let m = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        match m.find(|c| c == ':' || c == '=') {
            Some(pos) => format!("{}\"[redacted]\"", &m[..pos + 1]),
            None => m.to_string(),
        }
    })
    .into_owned();
    re(
        &REDACT_JSON_BARE,
        r#"(?i)"?(?:access_token|refresh_token|id_token|token|client_secret|code_verifier|authorization)"?\s*[:=]\s*[^\s&,}]+"#,
    )
    .replace_all(&t, |caps: &regex::Captures| {
        let m = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        match m.find(|c| c == ':' || c == '=') {
            Some(pos) => {
                let sep = &m[pos..pos + 1];
                format!("{}{}[redacted]", &m[..pos], sep)
            }
            None => m.to_string(),
        }
    })
    .into_owned()
}

pub fn safe_error(err: impl std::fmt::Display) -> String {
    redact_secrets(&err.to_string())
}

pub fn mask_email(email: Option<&str>) -> Option<String> {
    let email = email?;
    let (name, domain) = email.split_once('@')?;
    if name.is_empty() || domain.is_empty() {
        return Some("[redacted-email]".into());
    }
    let last = name.chars().last().unwrap_or('*');
    Some(format!(
        "{}***{}@{}",
        name.chars().next().unwrap_or('*'),
        last,
        domain
    ))
}

/// Only loopback binds are allowed for the OAuth callback.
pub fn resolve_callback_host() -> String {
    let host = crate::config::ag_env("CALLBACK_HOST")
        .unwrap_or_else(|| "127.0.0.1".into())
        .trim()
        .to_lowercase();
    match host.as_str() {
        "127.0.0.1" | "::1" | "localhost" => "127.0.0.1".to_string(),
        other => panic!(
            "Unsafe ANTIGRAVITY_CALLBACK_HOST=\"{other}\". Only loopback hosts are allowed: 127.0.0.1, ::1, localhost."
        ),
    }
}

/// Prevent token exfiltration via poisoned BASE_URL (SSRF / credential leak).
/// Loopback http is permitted only when AG_TEST_ALLOW_INSECURE_BASE=1 (tests).
pub fn assert_safe_api_base_url(raw: &str) -> Result<String, String> {
    let u = url::Url::parse(raw).map_err(|_| format!("Invalid ANTIGRAVITY_BASE_URL: {raw}"))?;
    let insecure_ok = std::env::var("AG_TEST_ALLOW_INSECURE_BASE").ok().as_deref() == Some("1");
    if insecure_ok {
        let host = u.host_str().unwrap_or("").to_lowercase();
        if u.scheme() == "http" && (host == "127.0.0.1" || host == "localhost" || host == "::1") {
            let mut s = u.as_str().to_string();
            while s.ends_with('/') {
                s.pop();
            }
            return Ok(s);
        }
    }
    if u.scheme() != "https" {
        return Err(format!(
            "ANTIGRAVITY_BASE_URL must use https (got {})",
            u.scheme()
        ));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("ANTIGRAVITY_BASE_URL must not include credentials".into());
    }
    let host = u.host_str().unwrap_or("").to_lowercase();
    let allowed = host == "googleapis.com"
        || host.ends_with(".googleapis.com")
        || host.ends_with(".sandbox.googleapis.com");
    if !allowed {
        return Err(format!(
            "ANTIGRAVITY_BASE_URL host \"{host}\" is not allowed. Use a *.googleapis.com endpoint."
        ));
    }
    let path = u.path().trim_end_matches('/').to_string();
    let origin = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
    let origin = match u.port() {
        Some(p) => format!("{origin}:{p}"),
        None => origin,
    };
    Ok(if path.is_empty() || path == "/" {
        origin
    } else {
        format!("{origin}{path}")
    })
}

/// Deterministic UUIDv5-shaped id from a seed (port of upstream stableUuid).
pub fn stable_uuid(seed: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// UUID-shaped stable project id from a seed (account email preferred).
pub fn stable_project_id(seed: &str) -> String {
    stable_uuid(&format!("antigravity:{seed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_bearer_and_json() {
        let out = redact_secrets(
            "Bearer ya29.abc123 and 1//0gLongRefreshTokenValueHere plus {\"access_token\":\"zzz\"}",
        );
        assert!(!out.contains("ya29.abc123"));
        assert!(!out.contains("0gLongRefreshTokenValueHere"));
        assert!(!out.contains("\"zzz\""));
        assert!(out.contains("[redacted]"));
    }

    #[test]
    fn stable_uuid_shape() {
        let u = stable_uuid("antigravity:conv:hello");
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[4].len(), 12);
        assert!(u.len() == 36);
        assert_eq!(stable_uuid("x"), stable_uuid("x"));
    }

    #[test]
    fn redacts_complete_values_including_spaces_and_escaped_quotes() {
        for input in [
            r#"{"client_secret":"super secret value"}"#,
            r#"token="escaped\"secret""#,
            "authorization=secret-value",
            r#"{"access_token":"zzz"}"#,
        ] {
            let out = redact_secrets(input);
            for secret in ["super secret value", "secret-value", "escaped", "zzz"] {
                assert!(!out.contains(secret), "leaked in {out}");
            }
        }
    }

    #[test]
    fn base_url_guard() {
        assert!(assert_safe_api_base_url("http://evil.com").is_err());
        assert!(assert_safe_api_base_url("https://user:pw@googleapis.com").is_err());
        assert!(assert_safe_api_base_url("https://evil.com/path").is_err());
        assert_eq!(
            assert_safe_api_base_url("https://daily-cloudcode-pa.googleapis.com/").unwrap(),
            "https://daily-cloudcode-pa.googleapis.com"
        );
    }
}
