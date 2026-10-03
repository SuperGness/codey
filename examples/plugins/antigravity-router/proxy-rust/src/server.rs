// HTTP/WS server: OpenAI Responses (SSE + non-streaming), WebSocket Responses
// contract (Codey local-router compatible), model catalog, health, usage,
// doctor, image generation. Port of proxy/server.mjs + lib/ws.js contract.
use crate::proxy_core::{self, ExecuteOutcome, RequestContext, Sink, SSE_END_SENTINEL};
use crate::ws::{accept_key, WsEvent, WsReader, WsWriter};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

type RespBody = BoxBody<Bytes, Infallible>;
type HttpResponse = hyper::Response<RespBody>;

pub struct AppState {
    pub ctx: RequestContext,
    pub port: u16,
}

fn full_body(v: Value) -> HttpResponse {
    response_with(
        200,
        [(hyper::header::CONTENT_TYPE, "application/json")],
        v.to_string().into_bytes(),
    )
}

fn response_with(
    status: u16,
    headers: impl IntoIterator<Item = (hyper::header::HeaderName, &'static str)>,
    body: Vec<u8>,
) -> HttpResponse {
    let mut builder = hyper::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Full::new(Bytes::from(body)).boxed())
        .expect("static response")
}

fn error_json(status: u16, message: &str) -> HttpResponse {
    full_body(json!({"error": {"message": message}})).tap_status(status)
}

trait TapStatus: Sized {
    fn tap_status(self, status: u16) -> HttpResponse;
}
impl TapStatus for HttpResponse {
    fn tap_status(self, status: u16) -> HttpResponse {
        let (mut parts, body) = self.into_parts();
        parts.status =
            hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR);
        HttpResponse::from_parts(parts, body)
    }
}

fn sse_response(rx: tokio::sync::mpsc::UnboundedReceiver<String>) -> HttpResponse {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Some(msg) if msg == SSE_END_SENTINEL => return None,
                Some(msg) => return Some((Ok::<_, Infallible>(Frame::data(Bytes::from(msg))), rx)),
                None => return None,
            }
        }
    });
    hyper::Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream; charset=utf-8")
        .header("Cache-Control", "no-cache")
        .header("Connection", "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(StreamBody::new(stream).boxed())
        .expect("sse response")
}

pub async fn run(state: Arc<AppState>, port: u16) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("bind 127.0.0.1:{port} failed: {e}"))?;
    crate::proxy_core::log_info(&format!("listening on http://127.0.0.1:{port}"));
    let endpoints = crate::discovery::endpoint_candidates().unwrap_or_default();
    crate::proxy_core::log_info(&format!("endpoints={}", endpoints.join(", ")));
    crate::discovery::hydrate_catalog_cache();

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                crate::proxy_core::log_warn(&format!("accept failed: {e}"));
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req: Request<Incoming>| {
                let state = state.clone();
                async move { route(state, req).await }
            });
            let builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            let _ = builder
                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                .await;
        });
    }
}

async fn route(
    state: Arc<AppState>,
    req: Request<Incoming>,
) -> Result<HttpResponse, std::convert::Infallible> {
    if req.headers().contains_key("origin") {
        return Ok(error_json(403, "browser origins are not allowed"));
    }
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !valid_local_host(host, state.port) {
        return Ok(error_json(403, "invalid local Host"));
    }
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let is_ws_upgrade = method == hyper::http::Method::GET
        && req
            .headers()
            .get("upgrade")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false)
        && (path == "/responses" || path == "/v1/responses");

    let result = if is_ws_upgrade {
        Ok(handle_websocket_upgrade(state, req).await)
    } else {
        match (&method, path.as_str()) {
            (&hyper::http::Method::GET, "/health") => handle_health(&state).await,
            (&hyper::http::Method::GET, "/models") | (&hyper::http::Method::GET, "/v1/models") => {
                handle_models(&state, req.uri().query()).await
            }
            (&hyper::http::Method::POST, "/responses") | (&hyper::http::Method::POST, "/v1/responses") => {
                handle_responses_post(&state, req).await
            }
            (&hyper::http::Method::POST, "/search") | (&hyper::http::Method::POST, "/v1/search") => handle_search(&state, req).await,
            (&hyper::http::Method::GET, "/usage") | (&hyper::http::Method::GET, "/v1/usage") => {
                handle_usage(&state).await
            }
            (&hyper::http::Method::GET, "/doctor") => Ok(full_body(json!({
                "ok": true,
                "service": "codey-antigravity-proxy",
                "diagnostics": crate::usage::snapshot_json()
            }))),
            (&hyper::http::Method::POST, "/v1/images/generations") | (&hyper::http::Method::POST, "/images/generations") => {
                handle_images(&state, req).await
            }
            (&hyper::http::Method::GET, "/") => Ok(response_with(
                200,
                [(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                b"antigravity-proxy (Rust)\nPOST /responses | /v1/responses (OpenAI Responses, SSE or JSON)\nGET /models | /v1/models\nPOST /v1/images/generations\nGET /usage | /v1/usage\nGET /doctor\nGET /health\n".to_vec(),
            )),
            _ => Ok(error_json(404, &format!("no route {path}"))),
        }
    };
    Ok(result.unwrap_or_else(|resp| resp))
}

fn valid_local_host(host: &str, port: u16) -> bool {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
    .iter()
    .any(|h| host.eq_ignore_ascii_case(h))
}

async fn handle_search(
    state: &Arc<AppState>,
    mut req: Request<Incoming>,
) -> Result<HttpResponse, HttpResponse> {
    let body = read_body_capped(&mut req, max_body_bytes()).await?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| error_json(400, "invalid JSON body"))?;
    let opts =
        crate::websearch::SearchOptions::from_value(&value).map_err(|e| error_json(400, &e))?;
    let (cred, token) =
        crate::auth::fresh_credential(&state.ctx.auth_path, &state.ctx.accounts_path)
            .await
            .map_err(|e| error_json(401, &crate::security::safe_error(e)))?;
    let project =
        crate::discovery::resolve_project_id(&token, None, cred.project_id, cred.email.as_deref());
    crate::websearch::execute_search(&token, &project, &opts, CancellationToken::new())
        .await
        .map(full_body)
        .map_err(|e| error_json(502, &crate::security::safe_error(e)))
}

#[cfg(test)]
mod route_guard_tests {
    #[test]
    fn rejects_rebinding_hosts_and_wrong_port() {
        for host in [
            "evil.test:8787",
            "127.0.0.1:9999",
            "127.0.0.1.evil:8787",
            "localhost",
            "localhost:8787@evil",
        ] {
            assert!(!super::valid_local_host(host, 8787));
        }
        assert!(super::valid_local_host("127.0.0.1:8787", 8787));
        assert!(super::valid_local_host("[::1]:8787", 8787));
    }
}

async fn read_body_capped(
    req: &mut Request<Incoming>,
    cap: usize,
) -> Result<Vec<u8>, HttpResponse> {
    let mut collected = Vec::new();
    while let Some(frame) =
        tokio::time::timeout(std::time::Duration::from_secs(30), req.body_mut().frame())
            .await
            .map_err(|_| error_json(408, "body read timeout"))?
    {
        let frame = frame.map_err(|_| error_json(400, "body read failed"))?;
        if let Some(bytes) = frame.data_ref() {
            if bytes.len() > cap.saturating_sub(collected.len()) {
                return Err(error_json(413, "body too large"));
            }
            collected.extend_from_slice(bytes);
        }
    }
    Ok(collected)
}

// --- POST /responses -------------------------------------------------------------

async fn handle_responses_post(
    state: &Arc<AppState>,
    mut req: Request<Incoming>,
) -> Result<HttpResponse, HttpResponse> {
    let cap = max_body_bytes();
    let body = read_body_capped(&mut req, cap).await?;
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Ok(error_json(400, "invalid JSON body")),
    };
    if !parsed.is_object() {
        return Ok(error_json(400, "request body must be a JSON object"));
    }
    let requested_model = parsed
        .get("model")
        .and_then(|m| m.as_str())
        .map(String::from)
        .unwrap_or_else(crate::default_model);
    let effort = crate::catalog::extract_effort(&parsed, &requested_model);
    let non_streaming = !parsed
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let cancel = CancellationToken::new();
    let sink = Sink::new(tx, cancel.clone());

    // Run the pipeline in a task; the channel carries SSE chunks.
    let ctx = state.ctx.clone();
    let task = tokio::spawn(async move {
        let mut parsed = parsed;
        proxy_core::execute_responses_request(
            &ctx,
            &mut parsed,
            &requested_model,
            effort,
            sink,
            if non_streaming { 0 } else { 30_000 },
            cancel,
        )
        .await
    });

    if !non_streaming {
        // Headers go out immediately; errors arrive as response.failed events
        // inside the stream (mirrors the Node proxy behaviour Codex expects).
        return Ok(sse_response(rx));
    }

    // Non-streaming: buffer events, extract the response.completed body.
    let mut buffer = String::new();
    while let Some(chunk) = rx.recv().await {
        if chunk == SSE_END_SENTINEL {
            break;
        }
        buffer.push_str(&chunk);
    }
    let outcome = task.await.unwrap_or(ExecuteOutcome {
        ok: false,
        status: 502,
        message: "execute task panicked".into(),
    });
    if !outcome.ok {
        return Ok(error_json(outcome.status, &outcome.message));
    }
    match parse_completed_response(&buffer) {
        Some(completed) => Ok(full_body(completed)),
        None => Ok(error_json(502, "upstream returned no completed response")),
    }
}

fn parse_completed_response(sse_text: &str) -> Option<Value> {
    for block in sse_text.split("\n\n") {
        let Some(line) = block.split('\n').find(|l| l.starts_with("data: ")) else {
            continue;
        };
        let Ok(evt) = serde_json::from_str::<Value>(&line[6..]) else {
            continue;
        };
        if evt.get("type").and_then(|t| t.as_str()) == Some("response.completed") {
            return evt.get("response").cloned();
        }
    }
    None
}

fn max_body_bytes() -> usize {
    let mb: usize = std::env::var("ANTIGRAVITY_MAX_BODY_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    mb.clamp(1, 512) * 1024 * 1024
}

// --- WS Responses contract ---------------------------------------------------------

const TERMINAL_RESPONSE_EVENTS: [&str; 4] = [
    "response.completed",
    "response.failed",
    "response.incomplete",
    "error",
];

fn ws_failed_frame(error: Value) -> Value {
    json!({
        "type": "response.failed",
        "response": {
            "id": format!("resp_codey_{}", crate::convert::rand_u64()),
            "object": "response",
            "created_at": chrono::Utc::now().timestamp(),
            "status": "failed",
            "output": [],
            "error": error,
            "incomplete_details": null
        }
    })
}

fn ws_route_error(status: u16, code: &str, message: &str) -> Value {
    json!({
        "type": "codey_route_error",
        "code": code,
        "message": message,
        "codey": {"httpStatus": status}
    })
}

/// Transform SSE chunks from the shared pipeline into bare JSON frames with the
/// terminal guard + stream_id re-attachment (port of createWsEventSink).
struct WsEventSink {
    buffer: String,
    stream_id: Option<String>,
    terminal: bool,
    tx: tokio::sync::mpsc::UnboundedSender<WsOut>,
    cancel: CancellationToken,
}

impl WsEventSink {
    fn feed(&mut self, chunk: &str) {
        self.buffer.push_str(chunk);
        while let Some(end) = self.buffer.find("\n\n") {
            let block: String = self.buffer.drain(..end + 2).collect();
            for line in block.split('\n') {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(mut evt) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                let is_bare_error = evt.get("type").and_then(|t| t.as_str()) == Some("error")
                    || (evt.get("type").and_then(|t| t.as_str()) == Some("response.failed")
                        && evt.get("response").is_none());
                if is_bare_error {
                    evt = ws_failed_frame(evt.get("error").cloned().unwrap_or(Value::Null));
                }
                if TERMINAL_RESPONSE_EVENTS
                    .contains(&evt.get("type").and_then(|t| t.as_str()).unwrap_or(""))
                {
                    if self.terminal {
                        continue;
                    }
                    self.terminal = true;
                }
                if self.stream_id.is_some()
                    && evt.get("stream_id").is_none()
                    && evt.pointer("/error/code").and_then(|c| c.as_str())
                        != Some("websocket_connection_limit_reached")
                {
                    evt["stream_id"] = json!(self.stream_id.clone().unwrap());
                }
                if self.tx.send(WsOut::Frame(evt.to_string())).is_err() {
                    self.cancel.cancel();
                }
            }
        }
    }
}

async fn handle_websocket_upgrade(state: Arc<AppState>, req: Request<Incoming>) -> HttpResponse {
    // Handshake headers.
    let Some(key) = req
        .headers()
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
    else {
        return error_json(400, "missing sec-websocket-key");
    };
    let version_ok = req
        .headers()
        .get("sec-websocket-version")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == "13")
        .unwrap_or(false);
    if !version_ok {
        return error_json(400, "Sec-WebSocket-Version: 13 required");
    }
    let accept = accept_key(&key);
    let mut req = req;
    let upgraded = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        match upgraded.await {
            Ok(upgraded) => run_websocket(state, TokioIo::new(upgraded)).await,
            Err(e) => crate::proxy_core::log_warn(&format!("ws upgrade failed: {e}")),
        }
    });
    hyper::Response::builder()
        .status(101)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Accept", accept)
        // Codey local-router contract header.
        .header("openai-beta", "responses_websockets=2026-02-06")
        .body(http_body_util::Empty::<Bytes>::new().boxed())
        .expect("101 response")
}

enum WsOut {
    Frame(String),
    Pong(Vec<u8>),
    Close(u16, String),
}

async fn run_websocket<S>(state: Arc<AppState>, stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = WsReader::new(read_half);
    let writer = Arc::new(tokio::sync::Mutex::new(WsWriter::new(write_half)));
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<WsOut>();

    // Writer task: data frames, pongs, and the final close.
    let writer_out = writer.clone();
    let out_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let mut w = writer_out.lock().await;
            match msg {
                WsOut::Frame(text) => {
                    if w.send_text(&text).await.is_err() {
                        break;
                    }
                }
                WsOut::Pong(payload) => {
                    let _ = w.send_pong(&payload).await;
                }
                WsOut::Close(code, reason) => {
                    let _ = w.send_close(code, &reason).await;
                    w.shutdown().await;
                    break;
                }
            }
        }
    });

    let mut queue: Vec<String> = Vec::new();
    let mut closing = false;
    loop {
        tokio::select! {
            ev = reader.next_event() => {
                match ev {
                    Err(e) => {
                        let _ = out_tx.send(WsOut::Close(e.code, e.reason));
                        break;
                    }
                    Ok(WsEvent::Ping(payload)) => {
                        let _ = out_tx.send(WsOut::Pong(payload));
                    }
                    Ok(WsEvent::Pong(_)) => {}
                    Ok(WsEvent::Closed(code, reason)) => {
                        // Echo the close code+reason verbatim.
                        let _ = out_tx.send(WsOut::Close(code, reason));
                        closing = true;
                        break;
                    }
                    Ok(WsEvent::Text(text)) => {
                        queue.push(text);
                    }
                }
            }
        }
        // Drain queued requests one at a time (router wait_for_upstream order).
        while !queue.is_empty() {
            let text = queue.remove(0);
            if let Err(close) = process_ws_request(&state, &out_tx, text).await {
                let _ = out_tx.send(WsOut::Close(close.0, close.1));
                closing = true;
                break;
            }
        }
        if closing {
            break;
        }
    }
    drop(out_tx);
    let _ = out_task.await;
}

/// Returns Err((code, reason)) when the connection must be closed.
async fn process_ws_request(
    state: &Arc<AppState>,
    out_tx: &tokio::sync::mpsc::UnboundedSender<WsOut>,
    text: String,
) -> Result<(), (u16, String)> {
    let send_error = |code: u16, err_code: &str, message: String| {
        let frame = ws_failed_frame(ws_route_error(code, err_code, &message));
        let _ = out_tx.send(WsOut::Frame(frame.to_string()));
    };

    let body: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            send_error(
                400,
                "invalid_request_body",
                format!("Responses WebSocket message is not valid JSON: {e}"),
            );
            return Ok(());
        }
    };
    if !body.is_object() {
        send_error(
            400,
            "invalid_request_body",
            "Responses WebSocket message must be a JSON object".into(),
        );
        return Ok(());
    }
    let mut body = body;
    let msg_type = body.get("type").and_then(|t| t.as_str()).map(String::from);
    if let Some(obj) = body.as_object_mut() {
        obj.remove("type");
    }
    if msg_type.as_deref() != Some("response.create") {
        send_error(
            400,
            "unsupported_websocket_message",
            "Responses WebSocket only supports response.create".into(),
        );
        return Ok(());
    }
    let mut stream_id: Option<String> = None;
    if let Some(id) = body.get("stream_id").cloned() {
        let valid = id
            .as_str()
            .map(|s| {
                (1..=256).contains(&s.len())
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            })
            .unwrap_or(false);
        if !valid {
            send_error(
                400,
                "invalid_stream_id",
                "Responses WebSocket stream_id must be 1-256 of a-z A-Z 0-9 _ - .".into(),
            );
            return Ok(());
        }
        stream_id = id.as_str().map(String::from);
        if let Some(obj) = body.as_object_mut() {
            obj.remove("stream_id");
        }
    }
    if let Some(s) = body.get("stream") {
        if s.as_bool() != Some(true) {
            send_error(
                400,
                "websocket_stream_required",
                "Responses WebSocket stream field can only be omitted or set to true".into(),
            );
            return Ok(());
        }
    }
    if let Some(b) = body.get("background") {
        if b.as_bool() == Some(true) {
            send_error(
                400,
                "websocket_background_unsupported",
                "Responses WebSocket does not support background mode".into(),
            );
            return Ok(());
        }
    }
    // Transport fields never reach conversion; always stream.
    if let Some(obj) = body.as_object_mut() {
        obj.remove("stream");
        obj.remove("background");
        obj.insert("stream".into(), json!(true));
    }

    let requested_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .map(String::from)
        .unwrap_or_else(crate::default_model);
    let effort = crate::catalog::extract_effort(&body, &requested_model);

    let cancel = CancellationToken::new();
    // Pipeline SSE chunks -> raw text pipe -> WsEventSink (terminal guard +
    // stream_id attach) -> outgoing frames directly.
    let (pipe_tx, mut pipe_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sink = Arc::new(tokio::sync::Mutex::new(WsEventSink {
        buffer: String::new(),
        stream_id,
        terminal: false,
        tx: out_tx.clone(),
        cancel: cancel.clone(),
    }));
    let sink_for_feed = sink.clone();
    let sink_after = sink.clone();
    let forward = tokio::spawn(async move {
        while let Some(chunk) = pipe_rx.recv().await {
            if chunk == SSE_END_SENTINEL {
                break;
            }
            sink_for_feed.lock().await.feed(&chunk);
        }
    });

    let psink = Sink::new(pipe_tx, cancel.clone());
    let mut parsed = body;
    let outcome = proxy_core::execute_responses_request(
        &state.ctx,
        &mut parsed,
        &requested_model,
        effort,
        psink,
        0,
        cancel,
    )
    .await;
    let _ = forward.await;
    if !sink_after.lock().await.terminal {
        let message: &str = if outcome.ok {
            "upstream stream ended without a terminal event"
        } else {
            &outcome.message
        };
        let frame = ws_failed_frame(ws_route_error(502, "websocket_proxy_failed", message));
        let _ = out_tx.send(WsOut::Frame(frame.to_string()));
    }
    Ok(())
}

// --- GET /models -------------------------------------------------------------------

async fn handle_models(
    state: &Arc<AppState>,
    query: Option<&str>,
) -> Result<HttpResponse, HttpResponse> {
    let force = query
        .map(|q| q.split('&').any(|kv| kv == "refresh=1" || kv == "force=1"))
        .unwrap_or(false);
    let ttl = crate::discovery::catalog_refresh_interval_ms();
    let cached = crate::discovery::cached_catalog();
    let fresh = cached
        .as_ref()
        .map(|c| !force && ttl > 0 && chrono::Utc::now().timestamp_millis() - c.checked_at < ttl)
        .unwrap_or(false);

    let (grouped, runtime_ids) = if fresh {
        let c = cached.unwrap();
        let ids: Vec<String> = c
            .catalog
            .pointer("/models")
            .and_then(|m| m.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        (c.grouped, ids)
    } else {
        match refresh_catalog(state).await {
            Ok(v) => v,
            Err(_) => {
                // Fallback: static public models only.
                let fallback = crate::catalog::fallback_catalog();
                (fallback, vec![])
            }
        }
    };

    let mut ids: Vec<String> = grouped.models.iter().map(|m| m.id.clone()).collect();
    for id in runtime_ids {
        if !ids.contains(&id) && crate::discovery::is_usable_runtime_model_id(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        ids.push(crate::default_model());
    }
    let data: Vec<Value> = ids
        .into_iter()
        .map(|id| json!({"id": id, "object": "model", "created": 0, "owned_by": "antigravity"}))
        .collect();
    Ok(full_body(json!({"object": "list", "data": data})))
}

async fn refresh_catalog(
    state: &Arc<AppState>,
) -> Result<(crate::catalog::Catalog, Vec<String>), ()> {
    let cred = crate::auth::read_auth_credential(&state.ctx.auth_path).map_err(|_| ())?;
    let token = crate::auth::fresh_credential(&state.ctx.auth_path, &state.ctx.accounts_path)
        .await
        .map(|(_, t)| t)
        .map_err(|_| ())?;
    let warmed = if cred.project_id.is_some() {
        None
    } else {
        crate::discovery::load_code_assist(&token).await
    };
    let project = crate::discovery::resolve_project_id(
        &token,
        warmed,
        cred.project_id.clone(),
        cred.email.as_deref(),
    );
    let catalog = crate::discovery::fetch_available_models_catalog(&token, &project)
        .await
        .map_err(|_| ())?;
    let fallback = crate::catalog::fallback_catalog();
    let grouped = crate::catalog::build_catalog(
        catalog.pointer("/models").unwrap_or(&Value::Null),
        &fallback,
    );
    let runtime_ids: Vec<String> = catalog
        .pointer("/models")
        .and_then(|m| m.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    crate::discovery::store_catalog(catalog, grouped.clone());
    Ok((grouped, runtime_ids))
}

// --- GET /health --------------------------------------------------------------------

async fn handle_health(state: &Arc<AppState>) -> Result<HttpResponse, HttpResponse> {
    match crate::auth::read_auth_credential(&state.ctx.auth_path) {
        Ok(cred) => {
            let fresh = cred.access_fresh();
            Ok(full_body(json!({
                "ok": true,
                "service": "codey-antigravity-proxy",
                "model": crate::default_model(),
                "authenticated": fresh,
                "endpoint": crate::discovery::endpoint_candidates().map(|e| e.first().cloned().unwrap_or_default()).unwrap_or_default(),
                "credentials": "present"
            })))
        }
        Err(e) => Ok(error_json(500, &format!("auth unavailable: {e}"))),
    }
}

// --- GET /usage ----------------------------------------------------------------------

async fn handle_usage(state: &Arc<AppState>) -> Result<HttpResponse, HttpResponse> {
    let cred = crate::auth::read_auth_credential(&state.ctx.auth_path)
        .map_err(|e| error_json(401, &format!("antigravity auth: {e}")))?;
    let (_, token) = crate::auth::fresh_credential(&state.ctx.auth_path, &state.ctx.accounts_path)
        .await
        .map_err(|e| error_json(401, &format!("antigravity auth: {e}")))?;
    let project = crate::discovery::resolve_project_id(
        &token,
        None,
        cred.project_id.clone(),
        cred.email.as_deref(),
    );
    match crate::usage::fetch_account_usage(&token, &project).await {
        Ok(usage) => Ok(full_body(usage)),
        Err(e) => Ok(error_json(502, &format!("usage fetch failed: {e}"))),
    }
}

// --- POST /v1/images/generations -------------------------------------------------------

async fn handle_images(
    state: &Arc<AppState>,
    mut req: Request<Incoming>,
) -> Result<HttpResponse, HttpResponse> {
    let body = read_body_capped(&mut req, max_body_bytes()).await?;
    let parsed: Value =
        serde_json::from_slice(&body).map_err(|_| error_json(400, "invalid JSON body"))?;
    let prompt = parsed
        .get("prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    if prompt.trim().is_empty() {
        return Ok(error_json(400, "prompt is required"));
    }
    let model = parsed.get("model").and_then(|m| m.as_str());
    let ratio = parsed
        .get("aspect_ratio")
        .or_else(|| parsed.get("ratio"))
        .and_then(|r| r.as_str())
        .map(String::from)
        .or_else(|| {
            // OpenAI-style size "1024x1792" -> closest supported ratio.
            parsed.get("size").and_then(|s| s.as_str()).and_then(|s| {
                let (w, h) = s.split_once('x')?;
                let w: f64 = w.parse().ok()?;
                let h: f64 = h.parse().ok()?;
                let r = w / h;
                let table: [(&str, f64); 10] = [
                    ("1:1", 1.0),
                    ("2:3", 2.0 / 3.0),
                    ("3:2", 1.5),
                    ("3:4", 0.75),
                    ("4:3", 4.0 / 3.0),
                    ("4:5", 0.8),
                    ("5:4", 1.25),
                    ("9:16", 0.5625),
                    ("16:9", 16.0 / 9.0),
                    ("21:9", 21.0 / 9.0),
                ];
                table
                    .iter()
                    .min_by(|a, b| (a.1 - r).abs().total_cmp(&(b.1 - r).abs()))
                    .map(|(id, _)| id.to_string())
            })
        })
        .unwrap_or_else(|| "1:1".to_string());

    let cred = crate::auth::read_auth_credential(&state.ctx.auth_path)
        .map_err(|e| error_json(401, &format!("antigravity auth: {e}")))?;
    let (_, token) = crate::auth::fresh_credential(&state.ctx.auth_path, &state.ctx.accounts_path)
        .await
        .map_err(|e| error_json(401, &format!("antigravity auth: {e}")))?;
    let warmed = if cred.project_id.is_some() {
        None
    } else {
        crate::discovery::load_code_assist(&token).await
    };
    let project = crate::discovery::resolve_project_id(
        &token,
        warmed,
        cred.project_id.clone(),
        cred.email.as_deref(),
    );

    let (images, _texts, used_model) = crate::imagegen::generate_image(
        &token,
        &project,
        &prompt,
        model,
        &ratio,
        CancellationToken::new(),
    )
    .await
    .map_err(|e| error_json(502, &e))?;
    let data: Vec<Value> = images
        .iter()
        .map(|img| json!({"b64_json": img.data, "mime_type": img.mime_type}))
        .collect();
    Ok(full_body(json!({
        "created": chrono::Utc::now().timestamp(),
        "model": used_model,
        "data": data
    })))
}
