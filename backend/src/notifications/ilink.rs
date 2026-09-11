//! Shared iLink (微信 ClawBot) client identity helpers used by both the login
//! flow in `commands::wechat_claw` and the notification channel adapter.
//! Keeping them here guarantees login, sync and delivery send the same
//! protocol headers.

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) fn random_wechat_uin() -> String {
    let uuid = Uuid::new_v4();
    let value = u32::from_be_bytes(uuid.as_bytes()[..4].try_into().expect("UUID prefix"));
    STANDARD.encode(value.to_string())
}

pub(crate) fn client_version() -> String {
    let mut components = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let major = components.next().unwrap_or(0) & 0xff;
    let minor = components.next().unwrap_or(0) & 0xff;
    let patch = components.next().unwrap_or(0) & 0xff;
    ((major << 16) | (minor << 8) | patch).to_string()
}

pub(crate) fn base_info() -> Value {
    json!({
        "channel_version": env!("CARGO_PKG_VERSION"),
        "bot_agent": format!("Codey/{}", env!("CARGO_PKG_VERSION")),
    })
}

/// Protocol headers for every iLink request. `token` is attached as a Bearer
/// credential only when non-empty. A token that is not a valid header value
/// (control characters, non-ASCII) is reported instead of panicking, so a
/// mis-pasted token cannot take down the watcher or the sync task.
pub(crate) fn headers(token: Option<&str>) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "AuthorizationType",
        "ilink_bot_token".parse().expect("static header"),
    );
    headers.insert(
        "X-WECHAT-UIN",
        random_wechat_uin().parse().expect("base64 header"),
    );
    headers.insert("iLink-App-Id", "bot".parse().expect("static header"));
    headers.insert(
        "iLink-App-ClientVersion",
        client_version().parse().expect("numeric header"),
    );
    if let Some(token) = token.filter(|token| !token.trim().is_empty()) {
        let value = format!("Bearer {token}")
            .parse()
            .map_err(|_| "微信 ClawBot 令牌包含非法字符，请重新扫码登录".to_string())?;
        headers.insert("Authorization", value);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_token_characters_return_an_error_instead_of_panicking() {
        let error = headers(Some("abc\r\ndef")).unwrap_err();
        assert!(error.contains("令牌"));
        assert!(
            headers(Some("valid-token"))
                .unwrap()
                .contains_key("authorization")
        );
        assert!(!headers(Some("   ")).unwrap().contains_key("authorization"));
    }
}
