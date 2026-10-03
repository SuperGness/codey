// Image generation through Cloud Code Assist (port of upstream src/image/image.ts).
// Shared by POST /v1/images/generations and the generate_image tool interception.
use crate::convert;
use crate::security::safe_error;
use base64::Engine;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const IMAGE_ASPECT_RATIOS: [&str; 10] = [
    "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
];

pub fn assert_safe_image_model(model_id: &str) -> Result<String, String> {
    let id = model_id.trim();
    if id.is_empty() || id.len() > 80 {
        return Err("Unsupported image model id.".into());
    }
    let ok = (id.starts_with("gemini-") && id.to_lowercase().contains("image"))
        || id.to_lowercase().starts_with("imagen-");
    if !ok {
        return Err(format!("Unsupported image model: {id}"));
    }
    Ok(id.to_string())
}

pub fn assert_safe_aspect_ratio(ratio: &str) -> Result<String, String> {
    let v = ratio.trim();
    if IMAGE_ASPECT_RATIOS.contains(&v) {
        return Ok(v.to_string());
    }
    Err(format!(
        "Unsupported aspect ratio: {v}. Use one of {}.",
        IMAGE_ASPECT_RATIOS.join(", ")
    ))
}

pub fn build_image_generate_request(
    prompt: &str,
    model: &str,
    project: &str,
    aspect_ratio: &str,
) -> String {
    let (request_id, session_id, _labels) =
        convert::antigravity_request_envelope(model, false, false, 1, "0".into(), 0, None, None);
    json!({
        "project": project,
        "model": model,
        "request": {
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "systemInstruction": {"role": "user", "parts": [{"text": crate::config::IMAGE_SYSTEM_INSTRUCTION}]},
            "generationConfig": {
                "imageConfig": {"aspectRatio": aspect_ratio},
                "candidateCount": 1
            }
        },
        "requestType": "agent",
        "userAgent": "antigravity",
        "requestId": request_id,
        "_session": session_id
    })
    .to_string()
}

pub struct GeneratedImage {
    pub data: String,
    pub mime_type: String,
}

fn image_extension(mime: &str) -> &'static str {
    let lower = mime.to_lowercase();
    if lower.contains("jpeg") || lower.contains("jpg") {
        "jpg"
    } else if lower.contains("webp") {
        "webp"
    } else if lower.contains("gif") {
        "gif"
    } else {
        "png"
    }
}

/// Collect inlineData parts from a full SSE body (non-streaming consumption).
fn collect_images_from_sse_text(text: &str) -> (Vec<GeneratedImage>, Vec<String>) {
    let mut images = Vec::new();
    let mut texts = Vec::new();
    for line in text.split('\n') {
        let Some(json_line) = line.strip_prefix("data:") else {
            continue;
        };
        let json_line = json_line.trim();
        if json_line.is_empty() || json_line == "[DONE]" {
            continue;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(json_line) else {
            continue;
        };
        let data = if chunk
            .get("response")
            .map(|r| r.is_object())
            .unwrap_or(false)
        {
            chunk["response"].clone()
        } else {
            chunk.clone()
        };
        let Some(candidates) = data.get("candidates").and_then(|c| c.as_array()) else {
            continue;
        };
        for candidate in candidates {
            let Some(parts) = candidate
                .pointer("/content/parts")
                .and_then(|p| p.as_array())
            else {
                continue;
            };
            for part in parts {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    texts.push(t.to_string());
                }
                if let Some(inline) = part.get("inlineData").filter(|i| i.is_object()) {
                    let d = inline.get("data").and_then(|v| v.as_str()).unwrap_or("");
                    if !d.is_empty() {
                        images.push(GeneratedImage {
                            data: d.to_string(),
                            mime_type: inline
                                .get("mimeType")
                                .and_then(|v| v.as_str())
                                .unwrap_or("image/png")
                                .to_string(),
                        });
                    }
                }
            }
        }
    }
    (images, texts)
}

/// Generate images, trying the model fallback chain across endpoint candidates.
/// Returns (images, text, model).
pub async fn generate_image(
    token: &str,
    project: &str,
    prompt: &str,
    model_pref: Option<&str>,
    aspect_ratio: &str,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(Vec<GeneratedImage>, Vec<String>, String), String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Image prompt is required.".into());
    }
    if prompt.len() > crate::config::MAX_PROMPT_CHARS {
        return Err(format!(
            "Image prompt is too long (max {} characters).",
            crate::config::MAX_PROMPT_CHARS
        ));
    }
    let ratio = assert_safe_aspect_ratio(aspect_ratio)?;
    let preferred =
        assert_safe_image_model(model_pref.unwrap_or(crate::config::DEFAULT_IMAGE_MODEL))?;
    let mut models: Vec<String> = vec![preferred.clone()];
    for id in crate::config::IMAGE_MODEL_FALLBACKS {
        if id != preferred {
            models.push(id.to_string());
        }
    }
    let endpoints = crate::discovery::endpoint_candidates()?;
    let client = reqwest::Client::new();
    let mut last_error = "no endpoint available".to_string();

    for model in &models {
        let body = build_image_generate_request(prompt, model, project, &ratio);
        for endpoint in &endpoints {
            if cancel.is_cancelled() {
                return Err("Request was aborted".into());
            }
            let url = format!("{endpoint}/v1internal:streamGenerateContent?alt=sse");
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .header(
                    "User-Agent",
                    crate::config::ag_env("USER_AGENT")
                        .unwrap_or_else(|| crate::config::DEFAULT_UA.into()),
                )
                .body(body.clone())
                .timeout(Duration::from_secs(300))
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_error = safe_error(e);
                    continue;
                }
            };
            let status = resp.status();
            if !status.is_success() {
                let text = crate::upstream::read_text_capped(resp, 65536, 30)
                    .await
                    .unwrap_or_else(|e| e);
                last_error = crate::client_util::json_or_text_error(&text)
                    .chars()
                    .take(400)
                    .collect();
                if status.as_u16() == 404
                    || matches!(status.as_u16(), 403 | 429 | 500 | 502 | 503 | 504)
                {
                    continue;
                }
                return Err(format!(
                    "Antigravity image request failed ({}): {last_error}",
                    status.as_u16()
                ));
            }
            let text = match crate::upstream::read_text_capped(resp, 64 * 1024 * 1024, 300).await {
                Ok(t) => t,
                Err(e) => {
                    last_error = safe_error(e);
                    continue;
                }
            };
            let (images, texts) = collect_images_from_sse_text(&text);
            if images.is_empty() {
                last_error = if texts.is_empty() {
                    "No image data returned.".to_string()
                } else {
                    texts.join(" ").trim().chars().take(400).collect()
                };
                continue;
            }
            return Ok((images, texts, model.clone()));
        }
    }
    Err(format!("Antigravity image generation failed: {last_error}"))
}

/// Save images under `base_dir` (default .pi/generated-images) with a
/// timestamped name; path traversal guard for user-provided subpaths.
pub fn save_generated_images(
    base_dir: &Path,
    requested: Option<&str>,
    images: &[GeneratedImage],
) -> Result<Vec<PathBuf>, String> {
    let root = base_dir
        .canonicalize()
        .unwrap_or_else(|_| base_dir.to_path_buf());
    let stamp = chrono::Utc::now().to_rfc3339().replace([':', '.'], "-");
    let mut saved = Vec::new();
    let many = images.len() > 1;
    for (index, image) in images.iter().enumerate() {
        let ext = image_extension(&image.mime_type);
        let suffix = if many {
            format!("-{}", index + 1)
        } else {
            String::new()
        };
        let default_name = format!("image-{stamp}{suffix}.{ext}");
        let target = match requested.map(str::trim).filter(|s| !s.is_empty()) {
            Some(rel) => {
                let joined = root.join(rel);
                if !joined.starts_with(&root) {
                    return Err("Image save path must be inside the working directory.".into());
                }
                if joined.extension().is_none() {
                    joined.join(default_name)
                } else {
                    joined
                }
            }
            None => root.join(&default_name),
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .map_err(|e| format!("invalid image base64: {e}"))?;
        std::fs::write(&target, bytes).map_err(|e| format!("write failed: {e}"))?;
        saved.push(target);
    }
    Ok(saved)
}

pub fn default_image_dir() -> PathBuf {
    std::env::var("AG_IMAGE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".pi").join("generated-images"))
}
