// Account usage / quota reporting + provider diagnostics
// (port of upstream src/usage/usage.ts + diagnostics.ts).
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

// --- Diagnostics snapshot (for /doctor) -------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct DiagnosticsSnapshot {
    pub status: Option<u16>,
    pub endpoint: Option<String>,
    pub error: Option<String>,
    pub project_id: Option<String>,
    pub resolved_runtime_model: Option<String>,
    pub latency_ms: Option<u64>,
    pub masked_email: Option<String>,
    pub token_expiry: Option<String>,
    pub tool_schema_warnings: Option<String>,
}

pub fn diagnostics() -> &'static Mutex<DiagnosticsSnapshot> {
    static SNAP: std::sync::OnceLock<Mutex<DiagnosticsSnapshot>> = std::sync::OnceLock::new();
    SNAP.get_or_init(|| Mutex::new(DiagnosticsSnapshot::default()))
}

pub fn set_diag<F: FnOnce(&mut DiagnosticsSnapshot)>(f: F) {
    if let Ok(mut snap) = diagnostics().lock() {
        f(&mut snap);
    }
}

pub fn snapshot_json() -> Value {
    match diagnostics().lock() {
        Ok(snap) => json!({
            "status": snap.status,
            "endpoint": snap.endpoint,
            "error": snap.error,
            "projectId": snap.project_id,
            "resolvedRuntimeModel": snap.resolved_runtime_model,
            "latencyMs": snap.latency_ms,
            "maskedEmail": snap.masked_email,
            "tokenExpiry": snap.token_expiry,
            "toolSchemaWarnings": snap.tool_schema_warnings
        }),
        Err(_) => json!({}),
    }
}

// --- Usage fetching -----------------------------------------------------------------

fn clamp_fraction(value: Option<f64>) -> Option<f64> {
    let v = value?;
    if !v.is_finite() {
        return None;
    }
    Some(v.clamp(0.0, 1.0))
}

pub async fn fetch_account_usage(token: &str, project: &str) -> Result<Value, String> {
    let assist_body = json!({"metadata": {"ideType": "ANTIGRAVITY", "platform": "PLATFORM_UNSPECIFIED", "pluginType": "GEMINI"}});
    let assist = crate::upstream::post_json("/v1internal:loadCodeAssist", token, &assist_body);
    let empty_body = json!({});
    let summary =
        crate::upstream::post_json("/v1internal:retrieveUserQuotaSummary", token, &empty_body);
    let models = crate::discovery::fetch_available_models_catalog(token, project);
    let (assist, summary, models) = futures_util::future::join3(assist, summary, models).await;

    let assist_data = assist.ok().map(|(_, _, d)| d).unwrap_or(Value::Null);
    let (groups, group_description, quota_summary_error) = match summary {
        Ok((_, _, data)) => parse_quota_summary(&data),
        Err(e) => (Vec::new(), None, Some(e)),
    };
    let models_data = models?;
    let parsed_models = parse_models(&models_data);

    let discovered_project = crate::discovery::extract_project_id(&assist_data);
    let resolved = crate::discovery::resolve_project_id(
        token,
        discovered_project,
        Some(project.to_string()),
        None,
    );

    let current_tier = assist_data
        .get("currentTier")
        .cloned()
        .unwrap_or(Value::Null);
    let paid_tier = assist_data.get("paidTier").cloned().unwrap_or(Value::Null);
    let plan_label = paid_tier
        .get("name")
        .and_then(|v| v.as_str())
        .map(|name| match paid_tier.get("id").and_then(|v| v.as_str()) {
            Some(id) => format!("{name} ({id})"),
            None => name.to_string(),
        })
        .or_else(|| {
            current_tier
                .get("name")
                .and_then(|v| v.as_str())
                .map(
                    |name| match current_tier.get("id").and_then(|v| v.as_str()) {
                        Some(id) => format!("{name} ({id})"),
                        None => name.to_string(),
                    },
                )
        });

    Ok(json!({
        "projectId": resolved,
        "productTier": current_tier,
        "paidTier": paid_tier,
        "planLabel": plan_label,
        "groups": groups,
        "groupDescription": group_description,
        "quotaSummaryError": quota_summary_error,
        "models": parsed_models,
        "defaultAgentModelId": models_data.get("defaultAgentModelId"),
        "fetchedAt": chrono::Utc::now().timestamp_millis()
    }))
}

fn parse_quota_summary(data: &Value) -> (Vec<Value>, Option<String>, Option<String>) {
    let mut groups = Vec::new();
    for group in data
        .get("groups")
        .and_then(|g| g.as_array())
        .into_iter()
        .flatten()
    {
        let mut buckets = Vec::new();
        for bucket in group
            .get("buckets")
            .and_then(|b| b.as_array())
            .into_iter()
            .flatten()
        {
            let remaining =
                clamp_fraction(bucket.get("remainingFraction").and_then(|v| v.as_f64()));
            let bucket_id = bucket
                .get("bucketId")
                .and_then(|v| v.as_str())
                .map(String::from);
            if remaining.is_none() && bucket_id.is_none() {
                continue;
            }
            buckets.push(json!({
                "bucketId": bucket_id.clone().unwrap_or_else(|| "unknown".into()),
                "displayName": bucket.get("displayName").and_then(|v| v.as_str())
                    .or(bucket_id.as_deref())
                    .unwrap_or("Limit"),
                "window": bucket.get("window").and_then(|v| v.as_str()),
                "resetTime": bucket.get("resetTime").and_then(|v| v.as_str()),
                "description": bucket.get("description").and_then(|v| v.as_str()),
                "remainingFraction": remaining.unwrap_or(0.0)
            }));
        }
        let display_name = group
            .get("displayName")
            .and_then(|v| v.as_str())
            .map(String::from);
        if buckets.is_empty() && display_name.is_none() {
            continue;
        }
        groups.push(json!({
            "displayName": display_name.unwrap_or_else(|| "Quota group".into()),
            "description": group.get("description").and_then(|v| v.as_str()),
            "buckets": buckets
        }));
    }
    (
        groups,
        data.get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        None,
    )
}

fn parse_models(data: &Value) -> Vec<Value> {
    let Some(models) = data.get("models").and_then(|m| m.as_object()) else {
        return vec![];
    };
    let mut rows: Vec<(String, Value)> = Vec::new();
    for (model_id, info) in models {
        if info
            .get("isInternal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        if model_id.starts_with("chat_") {
            continue;
        }
        let qi = info.get("quotaInfo").cloned().unwrap_or(Value::Null);
        rows.push((
            model_id.clone(),
            json!({
                "modelId": model_id,
                "displayName": info.get("displayName").and_then(|v| v.as_str())
                    .or(info.get("label").and_then(|v| v.as_str()))
                    .or(info.get("modelName").and_then(|v| v.as_str())),
                "remainingFraction": clamp_fraction(qi.get("remainingFraction").and_then(|v| v.as_f64())),
                "resetTime": qi.get("resetTime").and_then(|v| v.as_str()),
                "modelProvider": info.get("modelProvider").and_then(|v| v.as_str())
                    .or(info.get("apiProvider").and_then(|v| v.as_str())),
                "supportsThinking": info.get("supportsThinking").and_then(|v| v.as_bool()).unwrap_or(false),
                "supportsImages": info.get("supportsImages").and_then(|v| v.as_bool()).unwrap_or(false),
                "recommended": info.get("recommended").and_then(|v| v.as_bool()).unwrap_or(false)
            }),
        ));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.into_iter().map(|(_, v)| v).collect()
}

// --- Text formatting (CLI) ------------------------------------------------------------

fn remaining_percent(remaining: Option<f64>) -> Option<f64> {
    remaining.map(|r| (r * 1000.0).round() / 10.0)
}

fn progress_bar(remaining: Option<f64>, width: usize) -> String {
    match remaining {
        None => format!("[{}]", "?".repeat(width)),
        Some(r) => {
            let filled = ((r * width as f64).round() as usize).clamp(0, width);
            format!("[{}{}]", "#".repeat(filled), "-".repeat(width - filled))
        }
    }
}

fn format_reset(reset_time: Option<&str>) -> String {
    let Some(reset_time) = reset_time else {
        return "n/a".into();
    };
    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(reset_time) else {
        return reset_time.to_string();
    };
    let delta = ts.timestamp_millis() - chrono::Utc::now().timestamp_millis();
    if delta <= 0 {
        return "now".into();
    }
    let total_min = (delta / 60000).max(0) as u64;
    let days = total_min / (60 * 24);
    let hours = (total_min % (60 * 24)) / 60;
    let mins = total_min % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

pub fn format_usage_summary(usage: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(label) = usage.get("planLabel").and_then(|v| v.as_str()) {
        lines.push(label.to_string());
    }
    let groups = usage
        .get("groups")
        .and_then(|g| g.as_array())
        .cloned()
        .unwrap_or_default();
    if groups.is_empty() {
        if let Some(err) = usage.get("quotaSummaryError").and_then(|v| v.as_str()) {
            lines.push(quota_error_note(err));
        } else {
            lines.push("No quota groups returned.".into());
        }
        return lines.join("\n");
    }
    for group in &groups {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(
            group
                .get("displayName")
                .and_then(|v| v.as_str())
                .unwrap_or("Quota group")
                .to_string(),
        );
        if let Some(buckets) = group.get("buckets").and_then(|b| b.as_array()) {
            for bucket in buckets {
                let remaining = bucket.get("remainingFraction").and_then(|v| v.as_f64());
                let rem = remaining_percent(remaining)
                    .map(|p| format!("{p}%"))
                    .unwrap_or_else(|| "?%".into());
                lines.push(format!(
                    "  {} {}: {} left · resets {}",
                    progress_bar(remaining, 20),
                    bucket
                        .get("displayName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Limit"),
                    rem,
                    format_reset(bucket.get("resetTime").and_then(|v| v.as_str()))
                ));
            }
        }
    }
    lines.join("\n").trim_end().to_string()
}

fn quota_error_note(msg: &str) -> String {
    let re =
        regex::Regex::new(r"(?i)SUBSCRIPTION_REQUIRED|#3501|(?:lack|missing).*license").unwrap();
    if re.is_match(msg) {
        return "Aggregate quota summary needs a paid subscription (free-tier can't use that endpoint). Per-model usage is still available via `models`.".into();
    }
    format!(
        "Aggregate quota summary unavailable: {}",
        &msg[..msg.len().min(160)]
    )
}

pub fn format_models_list(usage: &Value, all: bool) -> String {
    let mut lines = vec![
        "Antigravity available models".to_string(),
        format!(
            "project={}",
            usage
                .get("projectId")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
        ),
    ];
    if let Some(default_model) = usage.get("defaultAgentModelId").and_then(|v| v.as_str()) {
        lines.push(format!("defaultAgentModel={default_model}"));
    }
    lines.push(String::new());

    let models = usage
        .get("models")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let rows: Vec<&Value> = models
        .iter()
        .filter(|m| {
            all || {
                let id = m.get("modelId").and_then(|v| v.as_str()).unwrap_or("");
                let re = regex::Regex::new(r"(?i)tab_|chat_").unwrap();
                !re.is_match(id)
            }
        })
        .collect();
    if rows.is_empty() {
        lines.push("No models returned.".into());
        return lines.join("\n");
    }
    let max_id = rows
        .iter()
        .map(|m| {
            m.get("modelId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .len()
        })
        .max()
        .unwrap_or(8);
    for m in rows {
        let id = m.get("modelId").and_then(|v| v.as_str()).unwrap_or("");
        let remaining = m.get("remainingFraction").and_then(|v| v.as_f64());
        let rem = remaining_percent(remaining)
            .map(|p| format!("{p}"))
            .unwrap_or_else(|| "  ?".into());
        let mut flags: Vec<String> = Vec::new();
        if m.get("recommended")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            flags.push("recommended".into());
        }
        if m.get("supportsThinking")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            flags.push("thinking".into());
        }
        if m.get("supportsImages")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            flags.push("images".into());
        }
        let display = m
            .get("displayName")
            .and_then(|v| v.as_str())
            .filter(|d| *d != id);
        lines.push(format!(
            "{:<width$}  rem {:>5}%  reset {:<8}{}{}",
            id,
            rem,
            format_reset(m.get("resetTime").and_then(|v| v.as_str())),
            if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(","))
            },
            display.map(|d| format!("  {d}")).unwrap_or_default(),
            width = max_id
        ));
    }
    lines.push(String::new());
    lines.push("Note: remaining % is pool-shared (not a private per-model budget).".to_string());
    lines.join("\n")
}

/// Friendly 4xx/5xx classification (port of friendlyAntigravityError).
pub fn friendly_antigravity_error(status: Option<u16>, text: &str) -> String {
    let status = match status {
        Some(s) => s,
        None => return crate::security::redact_secrets(text),
    };
    let msg = crate::security::redact_secrets(&crate::client_util::json_or_text_error(text));
    let msg = msg.chars().take(500).collect::<String>();
    let re = |pattern: &str| {
        regex::Regex::new(pattern)
            .map(|r| r.is_match(&msg))
            .unwrap_or(false)
    };
    match status {
        400 => {
            if re(r"(?i)Requests ending with a model turn are not supported") {
                "Antigravity rejected an invalid conversation message boundary. Retry with a new user message or start a new session.".into()
            } else if re(r"(?i)function call turn comes immediately after a user turn or after a function response turn") {
                "Antigravity rejected an invalid function-call message boundary. Start a new session and retry; re-login is not required.".into()
            } else if re(r"(?i)API key not valid|API_KEY_INVALID") {
                "Antigravity login expired or credentials are invalid. Run `login`, then retry.".into()
            } else if re(r"(?i)Invalid JSON payload|Unknown name") {
                format!("Antigravity request format was rejected by the backend ({msg}). Switch to a simpler model or retry.")
            } else if re(r"(?i)Request contains an invalid argument") {
                format!("Antigravity rejected this request ({msg}). Retry once; if it keeps failing, switch models or re-login.")
            } else {
                format!("Bad request from Antigravity. Retry once. Backend said: {msg}")
            }
        }
        401 => "Antigravity authentication failed. Run `login`, then retry.".into(),
        403 => {
            if re(r"(?i)permission|forbidden|access") {
                "Antigravity access was denied for this account or project. Try another model or account.".into()
            } else {
                format!("Antigravity denied this request. Re-login or try another model. Backend said: {msg}")
            }
        }
        404 => {
            if re(r"(?i)Requested entity was not found") {
                "This model is not available right now. Switch to gemini-3.8-flash, gemini-3.7-flash, gemini-3.6-flash, gemini-3.5-flash, gemini-3.1-pro, or another working model.".into()
            } else {
                format!("Antigravity could not find the requested resource. Retry or switch models. Backend said: {msg}")
            }
        }
        408 => "Antigravity timed out. Retry the same request.".into(),
        409 => "Antigravity reported a conflict for this request. Retry once or start a new chat session.".into(),
        429 => {
            let wait = regex::Regex::new(r"(?i)Resets? in ([^.\n]+)")
                .ok()
                .and_then(|r| r.captures(&msg))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim().to_string());
            if re(r"(?i)Individual quota reached") {
                format!("Quota reached. Please wait {}. Switch models or try again after reset.", wait.unwrap_or_else(|| "for reset".into()))
            } else {
                let hard_limit = wait.is_some()
                    || (!re(r"(?i)rate.?limit") && re(r"(?i)quota exceeded|exceeded your|limit reached|reached your|daily limit"));
                if hard_limit {
                    format!("Quota reached.{} Switch models or retry later.", wait.map(|w| format!(" Please wait {w}.")).unwrap_or_default())
                } else {
                    "Rate limited by Antigravity (429 ResourceExhausted). Retrying automatically; if it persists, switch models.".into()
                }
            }
        }
        500 => "Antigravity had an internal server error. Retry in a moment or switch models.".into(),
        502 => "Antigravity returned a bad gateway error. Retry in a moment.".into(),
        503 => {
            if re(r"(?i)No capacity available") {
                "This model has no capacity right now. Retry later or switch to another model.".into()
            } else {
                "Antigravity is temporarily unavailable. Retry in a moment or switch models.".into()
            }
        }
        504 => "Antigravity timed out upstream. Retry in a moment.".into(),
        _ => msg,
    }
}

/// Detect a hard quota wall (triggers account failover) vs transient throttling.
pub fn is_hard_quota_wall(text: &str) -> bool {
    let re = |p: &str| {
        regex::Regex::new(p)
            .map(|r| r.is_match(text))
            .unwrap_or(false)
    };
    re(r"(?i)Individual quota reached")
        || re(r"(?i)Resets? in ")
        || (!re(r"(?i)rate.?limit") && re(r"(?i)quota exceeded|exceeded your|daily limit"))
}

pub type UsageMap = HashMap<String, Value>;
