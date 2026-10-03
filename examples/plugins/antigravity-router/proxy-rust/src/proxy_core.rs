// Request orchestration: model routing, credential/project resolution, the
// upstream fetch loop (endpoint + runtime candidates, empty-stream retry,
// hard-quota account failover), and the multi-round web-search / image-tool
// interception. Port of proxy/server.mjs executeResponsesRequest + upstream
// streamAntigravity.
use crate::catalog::{self, Effort};
use crate::convert::{self, ConvertOptions};
use crate::stream::{pump_stream, InterceptedCall, PumpOptions};
use crate::upstream::{fetch_upstream_sse, stream_stall_timeout_ms};
use crate::writer::Writer;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub const RETRYABLE_STATUSES: [u16; 7] = [403, 404, 429, 500, 502, 503, 504];

#[derive(Clone)]
pub struct RequestContext {
    pub auth_path: std::path::PathBuf,
    pub accounts_path: std::path::PathBuf,
    pub image_mirror: Option<Arc<crate::images_in::BedMirror>>,
}

pub const SSE_END_SENTINEL: &str = "\u{0}__AG_PROXY_END__";

pub struct Sink {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
}

impl Sink {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<String>, cancel: CancellationToken) -> Sink {
        Sink { tx, cancel }
    }
    fn write(&self, chunk: &str) {
        if self.tx.send(chunk.to_string()).is_err() {
            // Receiver gone = client disconnected; stop upstream work.
            self.cancel.cancel();
        }
    }
    fn end(&self) {
        let _ = self.tx.send(SSE_END_SENTINEL.to_string());
    }
}

pub struct ExecuteOutcome {
    pub ok: bool,
    pub status: u16,
    pub message: String,
}

fn sig_remember(item: &Value, cache: &mut std::collections::HashMap<String, String>) {
    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
        if let (Some(call_id), Some(sig)) = (
            item.get("call_id").and_then(|c| c.as_str()),
            item.get("thought_signature").and_then(|s| s.as_str()),
        ) {
            if cache.len() > 500 {
                cache.clear();
            }
            cache.insert(call_id.to_string(), sig.to_string());
        }
    }
}

fn backfill_thought_signatures(
    parsed: &mut Value,
    cache: &std::collections::HashMap<String, String>,
) {
    if let Some(items) = parsed.get_mut("input").and_then(|i| i.as_array_mut()) {
        for item in items.iter_mut() {
            let is_call = item.get("type").and_then(|t| t.as_str()) == Some("function_call");
            if !is_call {
                continue;
            }
            let has_sig = item
                .get("thought_signature")
                .and_then(|s| s.as_str())
                .map(|s| crate::convert::is_valid_thought_signature(Some(s)))
                .unwrap_or(false);
            if has_sig {
                continue;
            }
            let call_id = item
                .get("call_id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let sig = cache
                .get(&call_id)
                .cloned()
                .unwrap_or_else(|| crate::config::PLACEHOLDER_THOUGHT_SIGNATURE.to_string());
            item["thought_signature"] = json!(sig);
        }
    }
}

/// Resolve the requested model to runtime candidates. Known public models route
/// instantly (fast TTFT); unknown ids fall back to dynamic discovery.
async fn resolve_runtime_candidates(
    requested: &str,
    effort: Option<Effort>,
    token: &str,
    project: &str,
) -> Vec<String> {
    if let Some(pinned) = crate::config::ag_env("RUNTIME_MODEL").filter(|s| !s.trim().is_empty()) {
        return vec![pinned];
    }
    if catalog::is_known_public_model(requested) {
        let mut candidates = vec![catalog::get_request_model_id(requested, effort)];
        if let Some(fallback) = catalog::get_fallback_runtime_model(&candidates[0], effort) {
            if fallback != candidates[0] {
                candidates.push(fallback);
            }
        }
        return candidates;
    }
    let dynamic = crate::discovery::fetch_available_runtime_model(token, project, requested)
        .await
        .filter(|id| {
            id.starts_with("gemini-") || id.starts_with("claude-") || id.starts_with("gpt-oss-")
        });
    vec![dynamic.unwrap_or_else(|| requested.to_string())]
}

pub async fn execute_responses_request(
    ctx: &RequestContext,
    parsed: &mut Value,
    requested_model: &str,
    explicit_effort: Option<Effort>,
    sink: Sink,
    heartbeat_ms: u64,
    cancel: CancellationToken,
) -> ExecuteOutcome {
    let started = Instant::now();
    let fail = |status: u16, message: String| -> ExecuteOutcome {
        let redacted = crate::security::redact_secrets(&message);
        crate::usage::set_diag(|d| {
            d.error = Some(redacted.clone());
            d.status = Some(status);
        });
        sink.write(&format!(
            "data: {}\n\n",
            json!({"type": "response.failed", "error": {"message": redacted}})
        ));
        sink.end();
        ExecuteOutcome {
            ok: false,
            status,
            message: redacted,
        }
    };

    // --- credentials + project ---------------------------------------------------
    let (cred, token) =
        match crate::auth::fresh_credential(&ctx.auth_path, &ctx.accounts_path).await {
            Ok(v) => v,
            Err(e) => return fail(401, format!("antigravity auth: {e}")),
        };
    crate::usage::set_diag(|d| {
        d.masked_email = crate::security::mask_email(cred.email.as_deref());
        if cred.expires_ms > 0 {
            d.token_expiry = Some(
                chrono::DateTime::from_timestamp_millis(cred.expires_ms)
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default(),
            );
        }
    });
    let warmed_project = if cred.project_id.is_some() {
        None
    } else {
        crate::discovery::load_code_assist(&token).await
    };
    let mut project = crate::discovery::resolve_project_id(
        &token,
        warmed_project,
        cred.project_id.clone(),
        cred.email.as_deref(),
    );
    crate::usage::set_diag(|d| d.project_id = Some(project.clone()));
    crate::upstream::prewarm_connection();

    // --- request preparation -------------------------------------------------------
    let mut signature_cache: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Some(items) = parsed.get("input").and_then(|i| i.as_array()) {
        for item in items {
            sig_remember(item, &mut signature_cache);
        }
    }
    backfill_thought_signatures(parsed, &signature_cache);

    let max_mb: usize = std::env::var("ANTIGRAVITY_MAX_BODY_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    let img_stats = crate::images_in::resolve_response_images(
        parsed,
        max_mb.clamp(1, 20) * 1024 * 1024,
        crate::images_in::DEFAULT_TIMEOUT_MS,
    )
    .await;
    if img_stats.resolved > 0 || img_stats.failed > 0 {
        crate::log_info(&format!(
            "images resolved={} failed={}",
            img_stats.resolved, img_stats.failed
        ));
        if let Some(mirror) = &ctx.image_mirror {
            let mut n = 0;
            if let Some(items) = parsed.get("input").and_then(|i| i.as_array()) {
                for item in items {
                    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                        for block in content {
                            if let Some(url) =
                                block.pointer("/image_url/url").and_then(|u| u.as_str())
                            {
                                if let Some((_, payload)) = url.split_once(";base64,") {
                                    mirror.mirror(payload, "image/png", n);
                                    n += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if std::env::var("AG_DEBUG").is_ok() {
        let path = std::env::temp_dir().join("ag-last-request.json");
        let _ = std::fs::write(&path, json!({"input_items": parsed.get("input").and_then(Value::as_array).map(Vec::len), "tool_count": parsed.get("tools").and_then(Value::as_array).map(Vec::len), "images_resolved": img_stats.resolved, "images_failed": img_stats.failed}).to_string());
    }

    let tools = parsed
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let wants_search = crate::config::extra_tool_enabled("SEARCH")
        && tools.iter().any(|t| {
            t.get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.starts_with("web_search"))
                .unwrap_or(false)
                || t.get("name").and_then(Value::as_str) == Some("google_search")
        })
        && tools
            .iter()
            .any(|t| t.get("type").and_then(|v| v.as_str()) == Some("function"));
    let image_tool_enabled = crate::config::extra_tool_enabled("IMAGE")
        && tools
            .iter()
            .any(|t| t.get("type").and_then(|v| v.as_str()) == Some("function"));

    // --- writer + heartbeat ---------------------------------------------------------
    let response_id = format!("resp_proxy_{}", chrono::Utc::now().timestamp_millis());
    let mut writer = Writer::new(
        response_id,
        requested_model.to_string(),
        chrono::Utc::now().timestamp(),
    );
    // Signatures learned from this response must survive into later requests, so
    // keep a process-wide cache keyed by call_id.
    let shared_sigs = signature_store();
    {
        let store = shared_sigs.clone();
        writer.saw_item = Some(Box::new(move |item: &Value| {
            if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                if let (Some(call_id), Some(sig)) = (
                    item.get("call_id").and_then(|c| c.as_str()),
                    item.get("thought_signature").and_then(|s| s.as_str()),
                ) {
                    if let Ok(mut map) = store.lock() {
                        if map.len() > 500 {
                            map.clear();
                        }
                        map.insert(call_id.to_string(), sig.to_string());
                    }
                }
            }
        }));
    }

    let heartbeat_task = if heartbeat_ms > 0 {
        let tx = sink_tx_clone(&sink);
        let cancel = cancel.clone();
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(heartbeat_ms));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {
                        if tx.send(": keep-alive\n\n".to_string()).is_err() {
                            cancel.cancel();
                            break;
                        }
                    }
                }
            }
        }))
    } else {
        None
    };

    let effort = explicit_effort.or_else(|| catalog::suffix_effort(requested_model));
    let mut runtime_candidates =
        resolve_runtime_candidates(requested_model, effort, &token, &project).await;
    let mut dynamic_retry_done = false;
    let endpoints = crate::discovery::endpoint_candidates().unwrap_or_default();
    let stall_deadline = stream_stall_timeout_ms();

    let mut active_token = token.clone();
    let mut tried_access_tokens: HashSet<String> = HashSet::new();
    tried_access_tokens.insert(token.clone());
    let mut searched: Vec<InterceptedCall> = Vec::new();
    let mut writer_started = false;
    let mut got_empty_success = false;
    let mut last_status: u16 = 502;
    let mut last_text = String::new();
    let mut runtime_in_use = runtime_candidates
        .first()
        .cloned()
        .unwrap_or_else(|| requested_model.to_string());

    // Outer loop: empty-stream retries (0..=2) + interception rounds + failover.
    let mut attempt: i32 = -1;
    let mut internal_tool_calls = 0usize;
    'outer: loop {
        attempt += 1;
        if attempt > 2 {
            break;
        }
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500 * 2u64.pow((attempt - 1) as u32))).await;
            if cancel.is_cancelled() {
                break;
            }
        }

        let mut cand_idx = 0usize;
        while cand_idx < runtime_candidates.len() {
            runtime_in_use = runtime_candidates[cand_idx].clone();
            crate::usage::set_diag(|d| d.resolved_runtime_model = Some(runtime_in_use.clone()));
            let opts = ConvertOptions {
                requested_model: requested_model.to_string(),
                effort,
                prompt: String::new(),
                image_tool: false,
                inject_image_tool: image_tool_enabled,
            };
            let gemini_body =
                match convert::responses_to_gemini_body(parsed, &project, &runtime_in_use, &opts) {
                    Ok(b) => b,
                    Err(e) => return fail(400, e),
                };

            // Endpoint candidates loop.
            let mut got_response: Option<reqwest::Response> = None;
            for endpoint in &endpoints {
                crate::usage::set_diag(|d| d.endpoint = Some(endpoint.clone()));
                match crate::upstream::fetch_upstream_sse_at(
                    &gemini_body,
                    &active_token,
                    &cancel,
                    false,
                    endpoint,
                )
                .await
                {
                    Err(e) => {
                        last_status = 502;
                        last_text = format!("antigravity upstream error: {e}");
                        crate::log_warn(&crate::security::redact_secrets(&last_text));
                        continue;
                    }
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        crate::usage::set_diag(|d| d.status = Some(status));
                        if (200..300).contains(&status) {
                            got_response = Some(resp);
                            break;
                        }
                        last_status = status;
                        last_text = crate::upstream::read_text_capped(resp, 65536, 30)
                            .await
                            .unwrap_or_else(|e| e);
                        let hard_quota =
                            status == 429 && crate::usage::is_hard_quota_wall(&last_text);
                        if hard_quota || !RETRYABLE_STATUSES.contains(&status) {
                            break;
                        }
                    }
                }
            }

            let Some(resp) = got_response else {
                // No OK response from any endpoint/runtime pair.
                if last_status == 404 {
                    if cand_idx + 1 < runtime_candidates.len() {
                        cand_idx += 1;
                        continue;
                    }
                    if catalog::is_known_public_model(requested_model) && !dynamic_retry_done {
                        dynamic_retry_done = true;
                        if let Some(dynamic) = crate::discovery::fetch_available_runtime_model(
                            &active_token,
                            &project,
                            requested_model,
                        )
                        .await
                        {
                            if !runtime_candidates.contains(&dynamic)
                                && (dynamic.starts_with("gemini-")
                                    || dynamic.starts_with("claude-")
                                    || dynamic.starts_with("gpt-oss-"))
                            {
                                runtime_candidates.push(dynamic);
                                cand_idx += 1;
                                continue;
                            }
                        }
                    }
                }
                // Hard quota wall -> failover to the next linked account.
                if last_status == 429 && crate::usage::is_hard_quota_wall(&last_text) {
                    if let Some((next_cred, next_token)) = crate::auth::failover_to_next_account(
                        &ctx.auth_path,
                        &ctx.accounts_path,
                        &tried_access_tokens,
                    )
                    .await
                    {
                        project = crate::discovery::resolve_project_id(
                            &next_token,
                            None,
                            next_cred.project_id,
                            next_cred.email.as_deref(),
                        );
                        tried_access_tokens.insert(next_token.clone());
                        active_token = next_token;
                        crate::log_info("hard quota wall: failed over to next linked account");
                        attempt = -1;
                        continue 'outer;
                    }
                }
                break;
            };

            // Pump the upstream stream (start only on the first successful pump).
            let mut collected_searches: Vec<InterceptedCall> = Vec::new();
            let mut collected_images: Vec<InterceptedCall> = Vec::new();
            let start_flag = !writer_started;
            let result = pump_stream(
                resp.bytes_stream(),
                &mut writer,
                &mut |chunk: &str| sink.write(chunk),
                PumpOptions {
                    start: start_flag,
                    hold_complete: wants_search || image_tool_enabled,
                    web_search_calls: if wants_search {
                        Some(&mut collected_searches)
                    } else {
                        None
                    },
                    image_calls: if image_tool_enabled {
                        Some(&mut collected_images)
                    } else {
                        None
                    },
                    cancel: cancel.clone(),
                    stall_timeout_ms: stall_deadline,
                },
            )
            .await;
            writer_started = true;
            merge_signatures(&shared_sigs, &mut signature_cache);
            if let Some(message) = result.error {
                sink.end();
                if let Some(task) = heartbeat_task {
                    task.abort();
                }
                return ExecuteOutcome {
                    ok: false,
                    status: 502,
                    message,
                };
            }

            // Interception rounds.
            let has_searches = wants_search
                && !collected_searches.is_empty()
                && searched.len() < crate::config::MAX_SEARCH_GROUNDS;
            let has_images = image_tool_enabled && !collected_images.is_empty();
            internal_tool_calls = internal_tool_calls
                .saturating_add(collected_searches.len())
                .saturating_add(collected_images.len());
            if internal_tool_calls > 16 {
                return fail(502, "Internal search/image tool call limit exceeded".into());
            }
            if has_searches || has_images {
                let mut replayed = false;
                for call in std::mem::take(&mut collected_searches) {
                    let digest = if call.tool_name == "google_search" {
                        match serde_json::from_str::<Value>(&call.query)
                            .map_err(|e| e.to_string())
                            .and_then(|v| crate::websearch::SearchOptions::from_value(&v))
                        {
                            Ok(opts) => match crate::websearch::execute_search(
                                &active_token,
                                &project,
                                &opts,
                                cancel.clone(),
                            )
                            .await
                            {
                                Ok(v) => v["result"].as_str().unwrap_or("").to_string(),
                                Err(e) => {
                                    format!("Search failed: {}", crate::security::safe_error(e))
                                }
                            },
                            Err(e) => format!("Search failed: {e}"),
                        }
                    } else {
                        crate::websearch::run_web_search(
                            &active_token,
                            &project,
                            &call.query,
                            &runtime_in_use,
                            cancel.clone(),
                        )
                        .await
                    };
                    searched.push(InterceptedCall {
                        tool_name: call.tool_name.clone(),
                        query: call.query.clone(),
                        call_id: call.call_id.clone(),
                        thought_signature: call.thought_signature.clone(),
                    });
                    append_tool_round(parsed, &call.tool_name, &call, &digest);
                    replayed = true;
                    crate::log_info(&format!(
                        "web search grounded: query_chars={} -> {}B",
                        call.query.chars().count(),
                        digest.len()
                    ));
                }
                for call in std::mem::take(&mut collected_images) {
                    let args: Value =
                        serde_json::from_str(&call.query).unwrap_or(json!({"prompt": call.query}));
                    let prompt = args
                        .get("prompt")
                        .and_then(|p| p.as_str())
                        .unwrap_or(&call.query)
                        .to_string();
                    let ratio = args
                        .get("aspect_ratio")
                        .and_then(|r| r.as_str())
                        .unwrap_or("1:1")
                        .to_string();
                    let digest = match crate::imagegen::generate_image(
                        &active_token,
                        &project,
                        &prompt,
                        None,
                        &ratio,
                        cancel.clone(),
                    )
                    .await
                    {
                        Ok((images, _texts, model)) => {
                            let dir = crate::imagegen::default_image_dir();
                            match crate::imagegen::save_generated_images(&dir, None, &images) {
                                Ok(paths) => {
                                    let list: Vec<String> =
                                        paths.iter().map(|p| p.display().to_string()).collect();
                                    crate::log_info(&format!(
                                        "generated image via {model}: {}",
                                        list.join(", ")
                                    ));
                                    format!(
                                        "Image generated with {} and saved to: {}",
                                        model,
                                        list.join(", ")
                                    )
                                }
                                Err(e) => {
                                    format!("Image generated with {} but saving failed: {e}", model)
                                }
                            }
                        }
                        Err(e) => format!("Image generation failed: {e}"),
                    };
                    append_tool_round(parsed, "generate_image", &call, &digest);
                    replayed = true;
                }
                if replayed {
                    // Fresh upstream call with the appended tool results; no
                    // response.created replay (writer already started).
                    attempt = -1;
                    continue 'outer;
                }
            }

            if !result.has_content {
                // Empty upstream stream: retry with backoff (or fail after 3).
                got_empty_success = true;
                continue;
            }
            if !(wants_search || image_tool_enabled) {
                // pump_stream already emitted the single terminal
                // response.completed (deferred to the stream tail so the final
                // usageMetadata lands in it).
                sink.end();
                if let Some(task) = heartbeat_task {
                    task.abort();
                }
                crate::usage::set_diag(|d| {
                    d.latency_ms = Some(started.elapsed().as_millis() as u64)
                });
                return ExecuteOutcome {
                    ok: true,
                    status: 200,
                    message: String::new(),
                };
            }
            // Interception mode: the pump only sealed the open item; emit the
            // terminal here (model made no further tool calls).
            writer.complete(result.usage.clone(), result.finish_reason.clone());
            writer.flush(&mut |chunk: &str| sink.write(chunk));
            sink.end();
            if let Some(task) = heartbeat_task {
                task.abort();
            }
            crate::usage::set_diag(|d| d.latency_ms = Some(started.elapsed().as_millis() as u64));
            return ExecuteOutcome {
                ok: true,
                status: 200,
                message: String::new(),
            };
        }
    }

    // All attempts exhausted.
    if last_status == 429 && crate::usage::is_hard_quota_wall(&last_text) {
        if let Some(next_token) = crate::auth::failover_to_next_account(
            &ctx.auth_path,
            &ctx.accounts_path,
            &tried_access_tokens,
        )
        .await
        {
            let (next_cred, next_token) = next_token;
            project = crate::discovery::resolve_project_id(
                &next_token,
                None,
                next_cred.project_id,
                next_cred.email.as_deref(),
            );
            tried_access_tokens.insert(next_token.clone());
            active_token = next_token;
            // Single failover retry with the current runtime model.
            let opts = ConvertOptions {
                requested_model: requested_model.to_string(),
                effort,
                prompt: String::new(),
                image_tool: false,
                inject_image_tool: image_tool_enabled,
            };
            if let Ok(gemini_body) =
                convert::responses_to_gemini_body(parsed, &project, &runtime_in_use, &opts)
            {
                if let Ok(resp) =
                    fetch_upstream_sse(&gemini_body, &active_token, &cancel, false).await
                {
                    if resp.status().is_success() {
                        let start_flag = !writer_started;
                        let result = pump_stream(
                            resp.bytes_stream(),
                            &mut writer,
                            &mut |chunk: &str| sink.write(chunk),
                            PumpOptions {
                                start: start_flag,
                                hold_complete: false,
                                web_search_calls: None,
                                image_calls: None,
                                cancel: cancel.clone(),
                                stall_timeout_ms: stall_deadline,
                            },
                        )
                        .await;
                        // hold_complete=false: pump emitted the terminal itself.
                        sink.end();
                        if let Some(task) = heartbeat_task {
                            task.abort();
                        }
                        crate::usage::set_diag(|d| {
                            d.latency_ms = Some(started.elapsed().as_millis() as u64)
                        });
                        return ExecuteOutcome {
                            ok: result.error.is_none() && result.has_content,
                            status: if result.error.is_none() && result.has_content {
                                200
                            } else {
                                502
                            },
                            message: result.error.unwrap_or_else(|| {
                                if result.has_content {
                                    String::new()
                                } else {
                                    "Antigravity returned an empty stream".into()
                                }
                            }),
                        };
                    }
                }
            }
        }
    }
    if got_empty_success && (200..300).contains(&last_status) {
        return fail(502, "Antigravity returned an empty stream".into());
    }
    let friendly = crate::usage::friendly_antigravity_error(Some(last_status), &last_text);
    fail(last_status, friendly)
}

fn signature_store() -> Arc<std::sync::Mutex<std::collections::HashMap<String, String>>> {
    static STORE: std::sync::OnceLock<
        Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    > = std::sync::OnceLock::new();
    STORE
        .get_or_init(|| Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())))
        .clone()
}

fn merge_signatures(
    store: &Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    into: &mut std::collections::HashMap<String, String>,
) {
    if let Ok(store) = store.lock() {
        for (k, v) in store.iter() {
            into.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
}

fn sink_tx_clone(sink: &Sink) -> tokio::sync::mpsc::UnboundedSender<String> {
    sink.tx.clone()
}

fn append_tool_round(parsed: &mut Value, name: &str, call: &InterceptedCall, output: &str) {
    if !parsed.get("input").map(|i| i.is_array()).unwrap_or(false) {
        let input = match parsed.get("input") {
            Some(Value::String(s)) => {
                json!([{"type": "message", "role": "user", "content": [{"type": "input_text", "text": s}]}])
            }
            _ => json!([]),
        };
        parsed["input"] = input;
    }
    let arguments = if name == "google_search" {
        call.query.clone()
    } else if name == "generate_image" {
        json!({"prompt": call.query}).to_string()
    } else {
        json!({"query": call.query}).to_string()
    };
    let mut fn_call = json!({
        "type": "function_call",
        "call_id": call.call_id,
        "name": name,
        "arguments": arguments
    });
    if let Some(sig) = &call.thought_signature {
        fn_call["thought_signature"] = json!(sig);
    }
    if let Some(items) = parsed.get_mut("input").and_then(|i| i.as_array_mut()) {
        items.push(fn_call);
        items.push(json!({
            "type": "function_call_output",
            "call_id": call.call_id,
            "name": name,
            "output": output
        }));
    }
}

#[cfg(test)]
mod round_tests {
    #[test]
    fn search_replay_preserves_directives() {
        let mut body = serde_json::json!({"input":"q"});
        let call = crate::stream::InterceptedCall {
            tool_name: "google_search".into(),
            query: r#"{"query":"q","thinking":true,"instruction":"focus"}"#.into(),
            call_id: "c".into(),
            thought_signature: None,
        };
        super::append_tool_round(&mut body, "google_search", &call, "answer");
        let args: serde_json::Value =
            serde_json::from_str(body["input"][1]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["thinking"], true);
        assert_eq!(args["instruction"], "focus");
    }
}

pub fn log_info(msg: &str) {
    eprintln!("[proxy] {msg}");
}

pub fn log_warn(msg: &str) {
    eprintln!("[proxy] {msg}");
}
