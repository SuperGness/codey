use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::json;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub(crate) const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const TURN_STATE_HEADER: &str = "x-codex-turn-state";

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub source_account_email: String,
    pub model: String,
    pub timeout: Duration,
}

pub(crate) struct State {
    pub cookie: HeaderValue,
    pub turn_state: HeaderValue,
}

pub(crate) async fn mint(
    client: &reqwest::Client,
    request: &Request,
) -> Result<State, &'static str> {
    let result = tokio::time::timeout(request.timeout, async {
        let credentials = super::transport::account_credentials(&request.source_account_email)
            .await
            .map_err(|_| "plugin_turn_state_account_failed")?;
        let body = json!({
            "model": request.model,
            "instructions": "",
            "stream": true,
            "store": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "ping"}]
            }],
            "reasoning": {"effort": "low"},
            "tool_choice": "auto",
            "parallel_tool_calls": false,
        });
        let session_id = Uuid::new_v4().to_string();
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", credentials.access_token))
                .map_err(|_| "plugin_turn_state_request_failed")?;
        authorization.set_sensitive(true);
        let account_id = HeaderValue::from_str(&credentials.upstream_account_id)
            .map_err(|_| "plugin_turn_state_request_failed")?;
        let response = client
            .post(ENDPOINT)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header("chatgpt-account-id", account_id)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header("originator", "codex-tui")
            .header("session-id", session_id)
            .header(reqwest::header::USER_AGENT, "codex_cli_rs/0.114.0")
            .json(&body)
            .send()
            .await
            .map_err(|_| "plugin_turn_state_request_failed")?;
        if !response.status().is_success() {
            return Err("plugin_turn_state_request_failed");
        }
        let headers = response.headers().clone();
        drop(response);
        parse_headers(&headers)
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => Err("plugin_turn_state_timeout"),
    }
}

fn parse_headers(headers: &HeaderMap) -> Result<State, &'static str> {
    let mut cflb = None;
    let mut oailb = None;
    for value in headers.get_all(reqwest::header::SET_COOKIE).iter() {
        let raw = value
            .to_str()
            .map_err(|_| "plugin_turn_state_invalid_cookie")?;
        let mut parts = raw.split(';');
        let pair = parts
            .next()
            .ok_or("plugin_turn_state_invalid_cookie")?
            .trim();
        let (name, cookie_value) = pair
            .split_once('=')
            .ok_or("plugin_turn_state_invalid_cookie")?;
        if !matches!(name, "__cflb" | "__oailb") {
            continue;
        }
        let cookie_value = cookie_value.trim();
        if cookie_value.is_empty()
            || cookie_value.len() > 8192
            || cookie_value.eq_ignore_ascii_case("deleted")
            || cookie_value
                .bytes()
                .any(|b| !matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
        {
            return Err("plugin_turn_state_invalid_cookie");
        }
        let mut max_age_expired = None;
        let mut expires_expired = false;
        for attribute in parts {
            let attribute = attribute.trim();
            if let Some((key, attr_value)) = attribute.split_once('=') {
                if key.eq_ignore_ascii_case("max-age") {
                    max_age_expired = Some(
                        attr_value
                            .trim()
                            .parse::<i64>()
                            .map(|age| age <= 0)
                            .unwrap_or(true),
                    );
                } else if key.eq_ignore_ascii_case("expires") {
                    expires_expired = chrono::DateTime::parse_from_rfc2822(attr_value.trim())
                        .map(|date| {
                            date.timestamp()
                                <= SystemTime::now()
                                    .duration_since(SystemTime::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs() as i64
                        })
                        .unwrap_or(true);
                } else if key.eq_ignore_ascii_case("domain") {
                    if !attr_value
                        .trim()
                        .trim_start_matches('.')
                        .eq_ignore_ascii_case("chatgpt.com")
                    {
                        return Err("plugin_turn_state_invalid_cookie");
                    }
                } else if key.eq_ignore_ascii_case("path") {
                    let path = attr_value.trim();
                    let request_path = "/backend-api/codex/responses";
                    if !path.starts_with('/')
                        || !(request_path == path
                            || request_path.starts_with(path)
                                && (path.ends_with('/')
                                    || request_path.as_bytes().get(path.len()) == Some(&b'/')))
                    {
                        return Err("plugin_turn_state_invalid_cookie");
                    }
                }
            }
        }
        // Max-Age 按 Cookie 规则优先于 Expires，不受属性顺序影响。
        if max_age_expired.unwrap_or(expires_expired) {
            return Err("plugin_turn_state_invalid_cookie");
        }
        match name {
            "__cflb" => cflb = Some(cookie_value.to_owned()),
            "__oailb" => oailb = Some(cookie_value.to_owned()),
            _ => {}
        }
    }
    let cflb = cflb.ok_or("plugin_turn_state_missing_cookie")?;
    let oailb = oailb.ok_or("plugin_turn_state_missing_cookie")?;
    let cookie = HeaderValue::from_str(&format!("__cflb={cflb}; __oailb={oailb}"))
        .map_err(|_| "plugin_turn_state_invalid_cookie")?;
    let turn_state = headers
        .get(TURN_STATE_HEADER)
        .ok_or("plugin_turn_state_missing_header")?
        .to_str()
        .map_err(|_| "plugin_turn_state_invalid_header")?
        .trim();
    if turn_state.is_empty()
        || turn_state.len() > 8192
        || turn_state.chars().any(|c| c.is_control())
    {
        return Err("plugin_turn_state_invalid_header");
    }
    let mut cookie = cookie;
    cookie.set_sensitive(true);
    let mut turn_state =
        HeaderValue::from_str(turn_state).map_err(|_| "plugin_turn_state_invalid_header")?;
    turn_state.set_sensitive(true);
    Ok(State { cookie, turn_state })
}
