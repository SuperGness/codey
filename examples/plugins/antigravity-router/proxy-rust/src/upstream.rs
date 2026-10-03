// Upstream HTTP helpers: headers, endpoint selection, SSE fetch with a
// response-header deadline (port of upstream fetchWithHeaderDeadline).
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub async fn read_text_capped(
    resp: reqwest::Response,
    cap: usize,
    timeout_secs: u64,
) -> Result<String, String> {
    tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let mut bytes = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(crate::security::safe_error)?;
            if chunk.len() > cap.saturating_sub(bytes.len()) {
                return Err("upstream body exceeds cap".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).map_err(|_| "upstream body is not UTF-8".into())
    })
    .await
    .map_err(|_| "upstream body timeout".to_string())?
}

pub fn antigravity_headers(token: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
        headers.insert(reqwest::header::AUTHORIZATION, v);
    }
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let ua =
        crate::config::ag_env("USER_AGENT").unwrap_or_else(|| crate::config::DEFAULT_UA.into());
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&ua) {
        headers.insert(reqwest::header::USER_AGENT, v);
    }
    headers
}

pub fn stream_header_timeout_ms() -> u64 {
    match crate::config::ag_env("STREAM_HEADER_TIMEOUT_MS").and_then(|v| v.parse::<u64>().ok()) {
        Some(v) => v,
        None => crate::config::STREAM_HEADER_TIMEOUT_DEFAULT_MS,
    }
}

pub fn stream_stall_timeout_ms() -> u64 {
    match crate::config::ag_env("STREAM_STALL_TIMEOUT_MS").and_then(|v| v.parse::<u64>().ok()) {
        Some(v) => v,
        None => crate::config::STREAM_STALL_TIMEOUT_DEFAULT_MS,
    }
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(8)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client")
    })
}

/// POST the Gemini streaming request. Honours the response-header deadline
/// (ANTIGRAVITY_STREAM_HEADER_TIMEOUT_MS) and the caller's cancellation token
/// (client disconnect). Mid-body stall guarding happens in the stream pump.
pub async fn fetch_upstream_sse(
    body: &str,
    token: &str,
    cancel: &CancellationToken,
    skip_header_deadline: bool,
) -> Result<reqwest::Response, String> {
    let endpoints = crate::discovery::endpoint_candidates()?;
    fetch_upstream_sse_at(
        body,
        token,
        cancel,
        skip_header_deadline,
        endpoints.first().ok_or("no endpoint candidates")?,
    )
    .await
}

pub async fn fetch_upstream_sse_at(
    body: &str,
    token: &str,
    cancel: &CancellationToken,
    skip_header_deadline: bool,
    endpoint: &str,
) -> Result<reqwest::Response, String> {
    let url = format!("{}/v1internal:streamGenerateContent?alt=sse", endpoint);
    let header_deadline = if skip_header_deadline {
        u64::MAX
    } else {
        stream_header_timeout_ms()
    };
    let request = http_client()
        .post(&url)
        .headers(antigravity_headers(token))
        .body(body.to_string());

    if header_deadline == u64::MAX {
        return tokio::select! {
            biased;
            _ = cancel.cancelled() => Err("request cancelled".into()),
            r = request.send() => r.map_err(|e| crate::security::safe_error(e)),
        };
    }
    let deadline = Duration::from_millis(header_deadline);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("request cancelled".into()),
        _ = tokio::time::sleep(deadline) => Err(format!("no response headers within {header_deadline}ms")),
        r = request.send() => r.map_err(|e| crate::security::safe_error(e)),
    }
}

/// POST a JSON body and return the parsed response (endpoint fallback chain,
/// continue-on retryable statuses). Port of upstream usage.rs postJson.
pub async fn post_json(
    path: &str,
    token: &str,
    body: &Value,
) -> Result<(String, u16, Value), String> {
    let endpoints = crate::discovery::endpoint_candidates()?;
    let mut last_error = String::new();
    for endpoint in &endpoints {
        let url = format!("{endpoint}{path}");
        let resp = http_client()
            .post(&url)
            .headers(antigravity_headers(token))
            .header("Accept", "application/json")
            .json(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await;
        match resp {
            Err(e) => {
                last_error = crate::security::safe_error(e);
                continue;
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = read_text_capped(resp, 4 * 1024 * 1024, 30).await?;
                let data: Value =
                    serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
                if (200..300).contains(&status) {
                    return Ok((endpoint.clone(), status, data));
                }
                last_error = data
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .map(String::from)
                    .unwrap_or(text);
                if !matches!(status, 403 | 404 | 429 | 500 | 502 | 503 | 504) {
                    return Err(format!(
                        "{path} failed ({status}): {}",
                        last_error.chars().take(300).collect::<String>()
                    ));
                }
            }
        }
    }
    Err(format!(
        "{path} failed: {}",
        if last_error.is_empty() {
            "no endpoint available".into()
        } else {
            last_error
        }
    ))
}

/// TLS pre-warm: open the connection on the first Antigravity request so the
/// first turn skips the handshake. Best-effort; runs at most once per process.
pub fn prewarm_connection() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    STARTED.get_or_init(|| {
        if crate::config::ag_env("NO_PREWARM").as_deref() == Some("1") {
            return;
        }
        let endpoints = match crate::discovery::endpoint_candidates() {
            Ok(e) => e,
            Err(_) => return,
        };
        let Some(primary) = endpoints.first().cloned() else {
            return;
        };
        tokio::spawn(async move {
            let req = http_client()
                .head(&primary)
                .timeout(Duration::from_secs(5))
                .send();
            let _ = req.await;
        });
    });
}
