//! Cold-start artwork served by the existing browser-level Fetch session.

use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use super::BrowserConnection;

pub(super) const DOCUMENT_URL: &str = "app://-/index.html";
/// 开屏动画脚本注册预算：冷启动若 `Page.enable` 卡顿则快速放弃，不长期挂起会话任务。
pub(super) const REGISTER_PLAYER_TIMEOUT: Duration = Duration::from_secs(3);
const ASSET_PREFIX: &str = "app://-/__codey-startup/";
const INTRO: &[u8] = include_bytes!("../../resources/startup-animation/intro.mp4");
const LOOP: &[u8] = include_bytes!("../../resources/startup-animation/loop.mp4");

pub(super) fn accepts_target_url(url: &str) -> bool {
    url.is_empty() || url == "about:blank" || url == DOCUMENT_URL
}

pub(super) fn asset_pattern() -> Value {
    json!({ "urlPattern": format!("{ASSET_PREFIX}*") })
}

pub(super) async fn register_player(
    connection: &BrowserConnection,
    session: &str,
) -> anyhow::Result<()> {
    // Enabling Page on this session makes the script run on the next document.
    connection
        .call_with_timeout(
            "Page.enable",
            json!({}),
            Some(session),
            REGISTER_PLAYER_TIMEOUT,
        )
        .await?;
    connection
        .call_with_timeout(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({
                "source": include_str!("startup_player.js"),
                "worldName": "codey-startup",
                "runImmediately": true,
            }),
            Some(session),
            REGISTER_PLAYER_TIMEOUT,
        )
        .await?;
    Ok(())
}

pub(super) fn asset_response(url: &str, params: &Value) -> Option<Value> {
    let (content, mime) = match url.strip_prefix(ASSET_PREFIX)? {
        "intro.mp4" => (INTRO, "video/mp4"),
        "loop.mp4" => (LOOP, "video/mp4"),
        _ => return None,
    };
    let request = &params["request"];
    if request["method"] != "GET" {
        return None;
    }
    let length = content.len();
    let mut headers = vec![
        json!({ "name": "Content-Type", "value": mime }),
        json!({ "name": "Cache-Control", "value": "no-store" }),
        json!({ "name": "Accept-Ranges", "value": "bytes" }),
    ];
    let range = request["headers"]
        .as_object()
        .and_then(|headers| {
            headers
                .iter()
                .find_map(|(name, value)| name.eq_ignore_ascii_case("range").then_some(value))
        })
        .and_then(Value::as_str);
    let (status, body) = match range {
        Some(range) => match byte_range(range, length) {
            Some((first, last)) => {
                headers.push(json!({
                    "name": "Content-Range", "value": format!("bytes {first}-{last}/{length}")
                }));
                (206, &content[first..=last])
            }
            None => {
                headers.push(json!({
                    "name": "Content-Range", "value": format!("bytes */{length}")
                }));
                (416, &[][..])
            }
        },
        None => (200, content),
    };
    headers.push(json!({ "name": "Content-Length", "value": body.len().to_string() }));
    Some(json!({
        "requestId": params["requestId"],
        "responseCode": status,
        "responseHeaders": headers,
        "body": base64::engine::general_purpose::STANDARD.encode(body),
    }))
}

fn byte_range(range: &str, length: usize) -> Option<(usize, usize)> {
    let (first, last) = range.strip_prefix("bytes=")?.split_once('-')?;
    let end = length - 1;
    if first.is_empty() {
        let suffix = last.parse::<usize>().ok()?;
        return (suffix > 0).then(|| (length.saturating_sub(suffix), end));
    }
    let first = first.parse::<usize>().ok()?;
    let last = if last.is_empty() {
        end
    } else {
        last.parse::<usize>().ok()?.min(end)
    };
    (first <= last).then_some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_media_serves_partial_responses_and_rejects_invalid_ranges() {
        let request = |range| {
            json!({
                "requestId": "video",
                "request": { "method": "GET", "headers": { "rAnGe": range } },
            })
        };
        let url = format!("{ASSET_PREFIX}intro.mp4");
        let part = asset_response(&url, &request("bytes=3-7")).unwrap();
        assert_eq!(part["responseCode"], 206);
        assert!(part["responseHeaders"].as_array().unwrap().contains(
            &json!({ "name": "Content-Range", "value": format!("bytes 3-7/{}", INTRO.len()) })
        ));
        assert_eq!(
            part["body"],
            base64::engine::general_purpose::STANDARD.encode(&INTRO[3..8])
        );
        assert_eq!(
            asset_response(&url, &request("bytes=0-")).unwrap()["responseCode"],
            206
        );
        assert_eq!(
            asset_response(&url, &request("bytes=-5")).unwrap()["responseCode"],
            206
        );
        assert_eq!(
            asset_response(&url, &request("bytes=9-3")).unwrap()["responseCode"],
            416
        );
        assert!(
            asset_response(&format!("{ASSET_PREFIX}other.mp4"), &request("bytes=0-")).is_none()
        );
    }
}
