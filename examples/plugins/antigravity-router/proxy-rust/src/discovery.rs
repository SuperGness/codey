// Client layer: endpoint candidates, project-id discovery (loadCodeAssist),
// dynamic runtime model resolution against fetchAvailableModels, and the
// catalog TTL cache (port of upstream src/client/client.ts + models/discovery.ts).
use crate::security::{safe_error, stable_project_id};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

pub const PROJECT_CACHE_TTL_MS: i64 = 30 * 60 * 1000;
pub const MODEL_CACHE_TTL_MS: i64 = 30 * 60 * 1000;
pub const DEFAULT_CATALOG_REFRESH_INTERVAL_MS: i64 = 4 * 60 * 60 * 1000;

pub fn endpoint_candidates() -> Result<Vec<String>, String> {
    match crate::config::ag_env("BASE_URL") {
        Some(explicit) if !explicit.trim().is_empty() => {
            Ok(vec![crate::security::assert_safe_api_base_url(
                explicit.trim(),
            )?])
        }
        _ => Ok(crate::config::ENDPOINT_FALLBACKS
            .iter()
            .map(|s| s.to_string())
            .collect()),
    }
}

pub fn json_or_text_error_text(text: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<Value>(text) {
        if let Some(msg) = parsed.pointer("/error/message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
    }
    text.to_string()
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client")
    })
}

// --- Project id discovery --------------------------------------------------------

pub fn extract_project_id(data: &Value) -> Option<String> {
    let direct = data
        .get("antigravityProjectId")
        .or_else(|| data.get("projectId"))
        .or_else(|| data.get("backendProjectId"))
        .or_else(|| data.get("userDefinedCloudaicompanionProject"))
        .or_else(|| data.get("cloudaicompanionProject"))
        .or_else(|| data.get("project"))?;
    if let Some(s) = direct.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(id) = direct.get("id").and_then(|v| v.as_str()) {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    for key in ["projects", "projectIds", "cloudaicompanionProjects"] {
        if let Some(arr) = data.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(found) = extract_project_id(item) {
                    return Some(found);
                }
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn default_project_id(seed_email: Option<&str>) -> String {
    if let Some(p) = crate::config::ag_env("PROJECT_ID").filter(|s| !s.trim().is_empty()) {
        return p;
    }
    stable_project_id(seed_email.unwrap_or("antigravity-default"))
}

pub fn resolve_project_id(
    token: &str,
    warmed_project: Option<String>,
    credential_project_id: Option<String>,
    email: Option<&str>,
) -> String {
    if let Some(p) = crate::config::ag_env("PROJECT_ID").filter(|s| !s.trim().is_empty()) {
        return p;
    }
    warmed_project
        .or(credential_project_id)
        .unwrap_or_else(|| default_project_id(email))
}

async fn list_cloud_ai_companion_projects(token: &str) -> Option<String> {
    for endpoint in endpoint_candidates().ok()? {
        let resp = http_client()
            .post(format!(
                "{endpoint}/v1internal:listCloudAICompanionProjects"
            ))
            .headers(crate::upstream::antigravity_headers(token))
            .json(&json!({}))
            .timeout(Duration::from_millis(crate::config::DISCOVERY_TIMEOUT_MS))
            .send()
            .await;
        if let Ok(resp) = resp {
            if resp.status().is_success() {
                if let Ok(data) = resp.json::<Value>().await {
                    if let Some(p) = extract_project_id(&data) {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

fn project_cache() -> &'static Mutex<HashMap<String, (Option<String>, i64)>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, (Option<String>, i64)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Discover the Cloud Code Assist project id with a short LRU cache keyed by
/// access token (port of loadCodeAssist).
pub async fn load_code_assist(token: &str) -> Option<String> {
    {
        let cache = project_cache().lock().ok()?;
        if let Some((project, expires)) = cache.get(token) {
            if *expires > chrono::Utc::now().timestamp_millis() {
                return project.clone();
            }
        }
    }
    let project = load_code_assist_uncached(token).await;
    if let Ok(mut cache) = project_cache().lock() {
        cache.insert(
            token.to_string(),
            (
                project.clone(),
                chrono::Utc::now().timestamp_millis() + PROJECT_CACHE_TTL_MS,
            ),
        );
        if cache.len() > 32 {
            if let Some(oldest) = cache.keys().next().cloned() {
                cache.remove(&oldest);
            }
        }
    }
    project
}

async fn load_code_assist_uncached(token: &str) -> Option<String> {
    let body = json!({"metadata": {"ideType": "ANTIGRAVITY"}});
    for endpoint in endpoint_candidates().ok()? {
        let resp = http_client()
            .post(format!("{endpoint}/v1internal:loadCodeAssist"))
            .headers(crate::upstream::antigravity_headers(token))
            .json(&body)
            .timeout(Duration::from_millis(crate::config::DISCOVERY_TIMEOUT_MS))
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(data) = resp.json::<Value>().await else {
            continue;
        };
        if let Some(p) = extract_project_id(&data) {
            return Some(p);
        }
        return list_cloud_ai_companion_projects(token).await;
    }
    None
}

// --- fetchAvailableModels --------------------------------------------------------

/// Merge fetchAvailableModels across endpoint candidates so daily/sandbox-only
/// models appear alongside production catalog entries.
pub async fn fetch_available_models_catalog(token: &str, project: &str) -> Result<Value, String> {
    let endpoints = endpoint_candidates()?;
    let tasks: Vec<_> = endpoints
        .iter()
        .map(|endpoint| {
            let endpoint = endpoint.clone();
            let token = token.to_string();
            let project = project.to_string();
            async move {
                let resp = http_client()
                    .post(format!("{endpoint}/v1internal:fetchAvailableModels"))
                    .headers(crate::upstream::antigravity_headers(&token))
                    .json(&json!({"project": project}))
                    .timeout(Duration::from_millis(crate::config::DISCOVERY_TIMEOUT_MS))
                    .send()
                    .await;
                match resp {
                    Err(e) => Err(safe_error(e)),
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        let text =
                            crate::upstream::read_text_capped(resp, 4 * 1024 * 1024, 8).await?;
                        let data: Value =
                            serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
                        if (200..300).contains(&status) {
                            Ok(data)
                        } else {
                            Err(json_or_text_error_text(&text))
                        }
                    }
                }
            }
        })
        .collect();
    let results = futures_util::future::join_all(tasks).await;
    let mut merged = serde_json::Map::new();
    let mut default_agent_model_id: Option<String> = None;
    let mut any_ok = false;
    for result in results {
        if let Ok(data) = result {
            any_ok = true;
            if let Some(models) = data.get("models").and_then(|m| m.as_object()) {
                for (k, v) in models {
                    merged.insert(k.clone(), v.clone());
                }
            }
            if let Some(id) = data.get("defaultAgentModelId").and_then(|v| v.as_str()) {
                default_agent_model_id = Some(id.to_string());
            } else if let Some(id) = data.get("defaultAgentModel").and_then(|v| v.as_str()) {
                default_agent_model_id = Some(id.to_string());
            }
        }
    }
    if !any_ok {
        return Err("/v1internal:fetchAvailableModels failed: no endpoint available".into());
    }
    crate::catalog::register_discovered_model_enums(
        &json!({"models": Value::Object(merged.clone())}),
    );
    Ok(json!({
        "models": Value::Object(merged),
        "defaultAgentModelId": default_agent_model_id
    }))
}

// --- Runtime model resolution ------------------------------------------------------

/// Runtime ids look like gemini-*/claude-*/gpt-oss-*, never MODEL_PLACEHOLDER_*.
pub fn is_usable_runtime_model_id(id: &str) -> bool {
    (id.starts_with("gemini-") || id.starts_with("claude-") || id.starts_with("gpt-oss-"))
        && !id.contains(' ')
        && !id.to_lowercase().starts_with("model_")
}

fn build_model_match_regex(requested_id: &str) -> regex::Regex {
    let req = requested_id.to_lowercase();
    let table: &[(&str, &str)] = &[
        (
            "gemini-3.8-flash-low",
            r"(?i)gemini[- ]3\.8[- ]flash \(low\)",
        ),
        (
            "gemini-3.8-flash-medium",
            r"(?i)gemini[- ]3\.8[- ]flash \(medium\)",
        ),
        (
            "gemini-3.8-flash-high",
            r"(?i)gemini[- ]3\.8[- ]flash \(high\)",
        ),
        (
            "gemini-3.7-flash-low",
            r"(?i)gemini[- ]3\.7[- ]flash \(low\)",
        ),
        (
            "gemini-3.7-flash-medium",
            r"(?i)gemini[- ]3\.7[- ]flash \(medium\)",
        ),
        (
            "gemini-3.7-flash-high",
            r"(?i)gemini[- ]3\.7[- ]flash \(high\)",
        ),
        (
            "gemini-3.6-flash-low",
            r"(?i)gemini[- ]3\.6[- ]flash \(low\)",
        ),
        (
            "gemini-3.6-flash-medium",
            r"(?i)gemini[- ]3\.6[- ]flash \(medium\)",
        ),
        (
            "gemini-3.6-flash-high",
            r"(?i)gemini[- ]3\.6[- ]flash \(high\)",
        ),
        (
            "gemini-3.5-flash-extra-low",
            r"(?i)gemini[- ]3\.5[- ]flash \(low\)",
        ),
        (
            "gemini-3.5-flash-low",
            r"(?i)gemini[- ]3\.5[- ]flash \(medium\)",
        ),
        (
            "gemini-3.5-flash-medium",
            r"(?i)gemini[- ]3\.5[- ]flash \(medium\)",
        ),
        (
            "gemini-3.5-flash-high",
            r"(?i)gemini[- ]3\.5[- ]flash \(high\)",
        ),
        (
            "gemini-3-flash-agent",
            r"(?i)gemini[- ]3\.5[- ]flash \(high\)",
        ),
        ("claude-opus-4-6", r"(?i)claude.*opus.*4\.6"),
        ("claude-sonnet-4-6", r"(?i)claude.*sonnet.*4\.6"),
        ("gpt-oss-120b", r"(?i)gpt.*oss.*120b"),
        ("gemini-3.1-pro-low", r"(?i)gemini[- ]3\.1[- ]pro \(low\)"),
        ("gemini-3.1-pro-high", r"(?i)gemini[- ]3\.1[- ]pro \(high\)"),
        ("gemini-pro-agent", r"(?i)gemini[- ]3\.1[- ]pro \(high\)"),
    ];
    if let Some((_, pattern)) = table.iter().find(|(id, _)| *id == req) {
        return regex::Regex::new(pattern).unwrap();
    }
    let escaped = regex::escape(&req).replace(r"\-", "[- ]");
    regex::Regex::new(&format!("(?i){escaped}")).unwrap()
}

fn find_dynamic_model(catalog: &Value, requested_id: &str) -> Option<String> {
    let models = catalog.get("models")?.as_object()?;
    if is_usable_runtime_model_id(requested_id) && models.contains_key(requested_id) {
        return Some(requested_id.to_string());
    }
    let target = build_model_match_regex(requested_id);
    for (model_id, info) in models {
        if !is_usable_runtime_model_id(model_id) {
            continue;
        }
        if target.is_match(model_id) {
            return Some(model_id.clone());
        }
        let label = info
            .get("label")
            .or_else(|| info.get("displayName"))
            .or_else(|| info.get("name"))
            .and_then(|l| l.as_str());
        if let Some(label) = label {
            if target.is_match(label) {
                return Some(model_id.clone());
            }
        }
    }
    None
}

fn model_cache() -> &'static Mutex<HashMap<String, (Option<String>, i64)>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, (Option<String>, i64)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a requested runtime model against fetchAvailableModels (30-min cache).
pub async fn fetch_available_runtime_model(
    token: &str,
    project: &str,
    requested: &str,
) -> Option<String> {
    let cache_key = format!("{token}::{project}::{requested}");
    if let Ok(cache) = model_cache().lock() {
        if let Some((hit, expires)) = cache.get(&cache_key) {
            if *expires > chrono::Utc::now().timestamp_millis() {
                return hit.clone();
            }
        }
    }
    let result = match fetch_available_models_catalog(token, project).await {
        Ok(catalog) => find_dynamic_model(&catalog, requested),
        Err(_) => None,
    };
    if let Ok(mut cache) = model_cache().lock() {
        cache.insert(
            cache_key,
            (
                result.clone(),
                chrono::Utc::now().timestamp_millis() + MODEL_CACHE_TTL_MS,
            ),
        );
        if cache.len() > 64 {
            let now = chrono::Utc::now().timestamp_millis();
            cache.retain(|_, (_, exp)| *exp > now);
        }
    }
    result
}

pub fn clear_caches() {
    if let Ok(mut cache) = project_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = model_cache().lock() {
        cache.clear();
    }
}

// --- Catalog TTL cache (for /models) ---------------------------------------------

#[derive(Clone)]
pub struct CatalogCache {
    pub catalog: Value,
    pub grouped: crate::catalog::Catalog,
    pub checked_at: i64,
}

fn catalog_cache() -> &'static Mutex<Option<CatalogCache>> {
    static CACHE: std::sync::OnceLock<Mutex<Option<CatalogCache>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub fn catalog_refresh_interval_ms() -> i64 {
    let raw = crate::config::ag_env("CATALOG_REFRESH_INTERVAL_MS")
        .or_else(|| crate::config::ag_env("REFRESH_INTERVAL_MS"));
    if let Some(v) = raw.and_then(|v| v.parse::<i64>().ok()) {
        if v >= 0 {
            return v;
        }
    }
    DEFAULT_CATALOG_REFRESH_INTERVAL_MS
}

pub fn cached_catalog() -> Option<CatalogCache> {
    catalog_cache().lock().ok().and_then(|c| c.clone())
}

pub fn store_catalog(catalog: Value, grouped: crate::catalog::Catalog) {
    let entry = CatalogCache {
        catalog,
        grouped,
        checked_at: chrono::Utc::now().timestamp_millis(),
    };
    if let Ok(mut cache) = catalog_cache().lock() {
        *cache = Some(entry);
    }
    // Last-known-good persistence for offline/cold start.
    if let Some(cache) = cached_catalog() {
        let path = crate::config::agent_dir().join("antigravity-catalog-cache.json");
        let snapshot = json!({
            "checkedAt": cache.checked_at,
            "catalog": cache.catalog,
            "grouped": {
                "models": cache.grouped.models.iter().map(|m| json!({
                    "id": m.id, "name": m.name, "reasoning": m.reasoning,
                    "levels": m.levels.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
                    "input_image": m.input_image,
                    "context_window": m.context_window, "max_tokens": m.max_tokens
                })).collect::<Vec<_>>(),
                "routing": cache.grouped.routing,
                "default_request": cache.grouped.default_request
            },
            "modelEnums": crate::catalog::snapshot_model_enums()
        });
        let _ = crate::auth::write_private_json(&path, &snapshot);
    }
}

/// Hydrate the last-known-good catalog from disk (cold start / offline).
pub fn hydrate_catalog_cache() {
    let path = crate::config::agent_dir().join("antigravity-catalog-cache.json");
    let Some(raw) = crate::auth::read_json_file(&path) else {
        return;
    };
    let checked_at = raw.get("checkedAt").and_then(|v| v.as_i64()).unwrap_or(0);
    if checked_at <= 0 {
        return;
    }
    if let Some(enums) = raw.get("modelEnums").and_then(|v| v.as_object()) {
        let map: HashMap<String, String> = enums
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
        crate::catalog::restore_model_enums(map);
    }
    let catalog = raw.get("catalog").cloned().unwrap_or(Value::Null);
    if catalog.is_null() {
        return;
    }
    let fallback = crate::catalog::fallback_catalog();
    let models: Vec<crate::catalog::PublicModel> = raw
        .pointer("/grouped/models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(crate::catalog::PublicModel {
                        id: m.get("id")?.as_str()?.to_string(),
                        name: m
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        reasoning: m.get("reasoning").and_then(|v| v.as_bool()).unwrap_or(true),
                        levels: m
                            .get("levels")
                            .and_then(|l| l.as_array())
                            .map(|ls| {
                                ls.iter()
                                    .filter_map(|l| l.as_str())
                                    .filter_map(crate::catalog::Effort::parse)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        input_image: m
                            .get("input_image")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true),
                        context_window: m
                            .get("context_window")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(128000),
                        max_tokens: m.get("max_tokens").and_then(|v| v.as_i64()).unwrap_or(8192),
                    })
                })
                .collect()
        })
        .unwrap_or(fallback.models);
    let grouped = crate::catalog::Catalog {
        models,
        routing: deserialize_string_map_map(raw.pointer("/grouped/routing")),
        default_request: deserialize_string_map(raw.pointer("/grouped/default_request")),
    };
    let entry = CatalogCache {
        catalog,
        grouped,
        checked_at,
    };
    if let Ok(mut cache) = catalog_cache().lock() {
        *cache = Some(entry);
    }
}

fn deserialize_string_map(v: Option<&Value>) -> HashMap<String, String> {
    v.and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn deserialize_string_map_map(v: Option<&Value>) -> HashMap<String, HashMap<String, String>> {
    v.and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, inner)| (k.clone(), deserialize_string_map(Some(inner))))
                .collect()
        })
        .unwrap_or_default()
}
