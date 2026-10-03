// Image input resolution: rewrite Responses input image blocks into canonical
// base64 data: URLs before Gemini conversion (port of proxy/lib/images.js).
// http(s) URLs are fetched server-side and inlined — the backend rejects URL
// forms with 400 (inlineData base64 is the only reliable transport).
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

pub const DEFAULT_MAX_BYTES: usize = 20 * 1024 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 20_000;

fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn image_source_of(block: &Value) -> Option<(String, Option<String>)> {
    // (raw, declared mime)
    let t = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if t == "input_image" {
        if let Some(u) = block.get("image_url") {
            match u {
                Value::String(s) => return Some((s.clone(), None)),
                Value::Object(_) => {
                    return Some((
                        u.get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        u.get("mime_type")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    ))
                }
                _ => {}
            }
        }
        if let Some(img) = block.get("image").filter(|i| i.is_object()) {
            return Some((
                img.get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                img.get("mime_type")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            ));
        }
    }
    if t == "input_file" {
        if let Some(fd) = block.get("file_data").and_then(|v| v.as_str()) {
            if fd.starts_with("data:image/") {
                return Some((fd.to_string(), None));
            }
        }
    }
    None
}

async fn fetch_as_base64(
    url: &str,
    max_bytes: usize,
    timeout_ms: u64,
) -> Result<(String, String), String> {
    let parsed = url::Url::parse(url).map_err(|_| "invalid image URL")?;
    let host = validated_host(&parsed)?;
    let port = parsed
        .port_or_known_default()
        .ok_or("image URL port is required")?;
    let addresses: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| "image DNS timeout")?
    .map_err(|_| "image DNS lookup failed")?
    .collect();
    if addresses.is_empty() || addresses.iter().any(|a| !is_public_ip(a.ip())) {
        return Err("image URL must resolve only to public addresses".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &addresses)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|_| "image fetch failed".to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let content_len = resp.content_length();
    let header_mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("image/png")
                .trim()
                .to_string()
        });
    if let Some(len) = content_len {
        if len as usize > max_bytes {
            return Err(format!("image exceeds {max_bytes} byte cap"));
        }
    }
    let mut stream = resp.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "image read failed")?;
        if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
            return Err(format!("image exceeds {max_bytes} byte cap"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let mime = sniff_mime(&bytes)
        .map(String::from)
        .or(header_mime)
        .unwrap_or_else(|| "image/png".to_string());
    Ok((
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        mime,
    ))
}

fn validated_host(url: &url::Url) -> Result<String, String> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("image URL must be HTTP(S) without credentials".into());
    }
    let host = url
        .host_str()
        .ok_or("image URL host is required")?
        .trim_matches(['[', ']'])
        .to_string();
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err("image URL address is not public".into());
        }
    }
    Ok(host)
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let s = ip.segments();
            // Only global unicast; block documentation, tunneling/translation and mapped ranges.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && (s[1] & 0xf000) == 0) // RFC 9637 documentation /20
        }
    }
}

pub struct ResolveStats {
    pub resolved: usize,
    pub failed: usize,
    pub notes: Vec<String>,
}

/// Walk parsed.input and rewrite image blocks in place to canonical
/// `{type:"input_image", image_url:{url:"data:<mime>;base64,<b64>"}}` form.
/// Per-block failures degrade to an input_text note; never fail the request.
pub async fn resolve_response_images(
    parsed: &mut Value,
    max_bytes: usize,
    timeout_ms: u64,
) -> ResolveStats {
    let mut stats = ResolveStats {
        resolved: 0,
        failed: 0,
        notes: vec![],
    };
    let Some(items) = parsed.get_mut("input").and_then(|i| i.as_array_mut()) else {
        return stats;
    };
    for item in items.iter_mut() {
        let content = match item.get_mut("content").and_then(|c| c.as_array_mut()) {
            Some(c) => c,
            None => continue,
        };
        for block in content.iter_mut() {
            let Some((raw, declared_mime)) = image_source_of(block) else {
                continue;
            };
            if raw.is_empty() {
                continue;
            }
            let lower = raw.to_lowercase();
            let result: Result<(String, String), String> =
                if lower.starts_with("http://") || lower.starts_with("https://") {
                    fetch_as_base64(&raw, max_bytes, timeout_ms).await
                } else if let Some(rest) = raw.strip_prefix("data:") {
                    match rest.find(',') {
                        Some(comma) => {
                            let prefix = &rest[..comma];
                            let mime = prefix
                                .split(';')
                                .next()
                                .filter(|m| !m.is_empty())
                                .map(String::from)
                                .or_else(|| declared_mime.clone())
                                .unwrap_or_else(|| "image/png".into());
                            let payload: String = rest[comma + 1..]
                                .chars()
                                .filter(|c| !c.is_whitespace())
                                .collect();
                            Ok((payload, mime))
                        }
                        None => Ok((
                            rest.chars().filter(|c| !c.is_whitespace()).collect(),
                            declared_mime.unwrap_or_else(|| "image/png".into()),
                        )),
                    }
                } else {
                    Ok((
                        raw.chars().filter(|c| !c.is_whitespace()).collect(),
                        declared_mime.unwrap_or_else(|| "image/png".into()),
                    ))
                };
            match result {
                Ok((payload, mime)) => {
                    stats.resolved += 1;
                    *block = json!({
                        "type": "input_image",
                        "image_url": {"url": format!("data:{mime};base64,{payload}")}
                    });
                }
                Err(reason) => {
                    stats.failed += 1;
                    stats.notes.push(reason.clone());
                    *block = json!({"type": "input_text", "text": format!("[image unavailable: {}]", reason)});
                }
            }
        }
    }
    stats
}

// --- Optional debug image-bed mirror (AG_IMAGE_MIRROR=1) ----------------------

pub struct BedMirror {
    api_key: String,
    client: reqwest::Client,
}

impl BedMirror {
    pub fn new() -> Option<BedMirror> {
        if std::env::var("AG_IMAGE_MIRROR").ok().as_deref() != Some("1") {
            return None;
        }
        let api_key = std::env::var("HFSY_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())?;
        Some(BedMirror {
            api_key,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .ok()?,
        })
    }

    /// Side upload only; never affects the request sent upstream.
    pub fn mirror(&self, payload_b64: &str, mime: &str, index: usize) {
        let ext = if mime.contains("jpeg") {
            "jpg"
        } else if mime.contains("webp") {
            "webp"
        } else {
            "png"
        };
        let filename = format!(
            "img-{}-{index}.{ext}",
            chrono::Utc::now().timestamp_millis()
        );
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let body = json!({"base64": payload_b64, "mime_type": mime, "filename": filename});
        tokio::spawn(async move {
            let _ = client
                .post("https://www.hfsyapi.cn/v1/files/image-upload")
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&body)
                .send()
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_metadata_and_mapped_addresses_rejected() {
        for host in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "2001:db8::1",
            "2002:a00:1::1",
            "3fff::1",
            "3fff:fff::1",
            "192.88.99.1",
        ] {
            assert!(!is_public_ip(host.parse().unwrap()), "{host}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
        assert!(
            validated_host(&url::Url::parse("https://user:pw@example.com/a").unwrap()).is_err()
        );
    }
}
