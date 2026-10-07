use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, RwLock, Weak},
    time::Duration,
};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode, body::Incoming, server::conn::http1, service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    sync::{Mutex as AsyncMutex, Semaphore},
    task::JoinSet,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};

use super::{auth::Auth, desktop::Desktop};
use crate::commands::AppState;

type Body = UnsyncBoxBody<Bytes, Infallible>;
type Reply = Response<Body>;
const MAX_BODY: usize = 1024 * 1024;
const COOKIE: &str = "codey_remote";
const HTML: &str = "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"UTF-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1,viewport-fit=cover,interactive-widget=resizes-content\"><meta name=\"color-scheme\" content=\"light dark\"><title>Codex · Codey 远程控制</title><link rel=\"stylesheet\" href=\"/remote.css\"></head><body><div id=\"root\"></div><script src=\"/remote.js\" defer></script></body></html>";

pub(super) struct Core {
    pub auth: Mutex<Auth>,
    pub state: Weak<AppState>,
    pub public_url: RwLock<Option<String>>,
    pub address: SocketAddr,
    pub actions: AsyncMutex<HashMap<String, ([u8; 32], Value)>>,
    pub shutdown: tokio::sync::watch::Sender<bool>,
    pub streams: Arc<Semaphore>,
}

pub(super) fn validate_public_url(value: &str) -> Result<Option<String>, String> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let url = reqwest::Url::parse(value.trim()).map_err(|_| "外网地址须为有效 HTTPS 地址")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("外网地址须为不含路径、参数和凭据的 HTTPS 地址".into());
    }
    Ok(Some(url.origin().ascii_serialization()))
}

fn allowed_origin(core: &Core, origin: &str) -> bool {
    if core
        .public_url
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        == Some(origin)
    {
        return true;
    }
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if url.scheme() != "http"
        || url.port_or_known_default() != Some(core.address.port())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    let host = url.host_str().unwrap_or_default();
    if host == "localhost" {
        return true;
    }
    match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
                || IpAddr::V4(ip) == core.address.ip()
        }
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || IpAddr::V6(ip) == core.address.ip()
        }
        _ => false,
    }
}

fn request_origin(core: &Core, request: &Request<Incoming>) -> Result<bool, &'static str> {
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .ok_or("请求缺少主机地址")?;
    let http_origin = format!("http://{host}");
    let https_origin = format!("https://{host}");
    let secure = core
        .public_url
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        == Some(&https_origin);
    if !secure && !allowed_origin(core, &http_origin) {
        return Err("不允许的访问主机");
    }
    if let Some(origin) = request.headers().get("origin") {
        let origin = origin.to_str().map_err(|_| "请求来源无效")?;
        if origin != if secure { &https_origin } else { &http_origin } {
            return Err("不允许跨站访问远程控制");
        }
    } else if request.method() != hyper::Method::GET {
        return Err("操作请求缺少来源");
    }
    if request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site")
    {
        return Err("不允许跨站访问远程控制");
    }
    Ok(secure)
}

fn session_token(request: &Request<Incoming>) -> String {
    request
        .headers()
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_string())
        .unwrap_or_default()
}

fn authorized(core: &Core, token: &str) -> bool {
    core.auth
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .authorized(token)
}

fn response(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Reply {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(body.into()).boxed_unsync())
        .expect("static response headers")
}

fn json_response(status: StatusCode, body: Value) -> Reply {
    response(status, "application/json; charset=utf-8", body.to_string())
}

fn failure(status: StatusCode, message: &str) -> Reply {
    json_response(status, json!({"status":"failed","message":message}))
}

pub(super) async fn serve(listener: TcpListener, core: Arc<Core>) {
    let permits = Arc::new(Semaphore::new(32));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream,_)) = accepted else { tokio::time::sleep(Duration::from_millis(100)).await; continue; };
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else { continue; };
                let core = Arc::clone(&core);
                connections.spawn(async move {
                    let _permit = permit;
                    let service = service_fn(move |request| handle(Arc::clone(&core),request));
                    let _ = http1::Builder::new().timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10))
                        .max_buf_size(16 * 1024).serve_connection(TokioIo::new(stream),service).with_upgrades().await;
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

// A boxed service boundary also breaks the recursive type from the local
// command dispatcher that starts this server and the panel commands it serves.
fn handle(
    core: Arc<Core>,
    request: Request<Incoming>,
) -> futures_util::future::BoxFuture<'static, Result<Reply, Infallible>> {
    Box::pin(async move {
        let mut reply = route(core, request).await;
        let headers = reply.headers_mut();
        for (name, value) in [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("x-content-type-options", "nosniff"),
            ("x-frame-options", "DENY"),
            (
                "content-security-policy",
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
            ),
        ] {
            headers.insert(
                hyper::header::HeaderName::from_static(name),
                hyper::header::HeaderValue::from_static(value),
            );
        }
        Ok(reply)
    })
}

async fn read_json(request: Request<Incoming>) -> Result<Value, &'static str> {
    if request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.split(';').next() != Some("application/json"))
    {
        return Err("请求须使用 JSON");
    }
    let mut body = request.into_body();
    let read = async {
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| "读取远程请求失败")?;
            if let Some(data) = frame.data_ref() {
                if bytes.len() + data.len() > MAX_BODY {
                    return Err("远程请求过大");
                }
                bytes.extend_from_slice(data);
            }
        }
        serde_json::from_slice(&bytes).map_err(|_| "远程请求 JSON 无效")
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .map_err(|_| "读取远程请求超时")?
}

async fn route(core: Arc<Core>, mut request: Request<Incoming>) -> Reply {
    let secure = match request_origin(&core, &request) {
        Ok(secure) => secure,
        Err(error) => return failure(StatusCode::FORBIDDEN, error),
    };
    let token = session_token(&request);
    let path = request.uri().path().to_string();
    let get = request.method() == hyper::Method::GET;
    if get {
        match path.as_str() {
            "/" => return response(StatusCode::OK, "text/html; charset=utf-8", HTML),
            "/remote.js" => {
                return response(
                    StatusCode::OK,
                    "text/javascript; charset=utf-8",
                    include_str!("../../../dist-overlay/codey-remote.js"),
                );
            }
            "/remote.css" => {
                return response(
                    StatusCode::OK,
                    "text/css; charset=utf-8",
                    include_str!("../../../dist-overlay/codey-remote.css"),
                );
            }
            _ => {}
        }
    }
    let pairing = path == "/remote/pair" && request.method() == hyper::Method::POST;
    if !pairing && !authorized(&core, &token) {
        return failure(StatusCode::UNAUTHORIZED, "连接已失效，请在电脑端重新配对");
    }
    if get && path == "/remote/session" {
        return json_response(StatusCode::OK, json!({"status":"ok"}));
    }
    if get && let Some(id) = path.strip_prefix("/remote/events/") {
        if super::desktop::validate_thread(id).is_err() {
            return failure(StatusCode::BAD_REQUEST, "会话标识无效");
        }
        return event_stream(core, token, id.to_string(), &mut request);
    }
    if request.method() != hyper::Method::POST {
        return failure(StatusCode::METHOD_NOT_ALLOWED, "请求方法不支持");
    }
    let args = match read_json(request).await {
        Ok(args) if args.is_object() => args,
        Ok(_) => return failure(StatusCode::BAD_REQUEST, "远程请求须为 JSON 对象"),
        Err(error) => return failure(StatusCode::BAD_REQUEST, error),
    };
    if pairing {
        let paired = core.auth.lock().unwrap_or_else(|e| e.into_inner()).pair(
            args["code"].as_str().unwrap_or_default(),
            args["name"].as_str().unwrap_or_default(),
        );
        return match paired {
            Ok(token) => {
                let mut reply = json_response(StatusCode::OK, json!({"status":"ok"}));
                let cookie = format!(
                    "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=43200{}",
                    if secure { "; Secure" } else { "" }
                );
                reply
                    .headers_mut()
                    .insert("set-cookie", cookie.parse().expect("generated cookie"));
                reply
            }
            Err(error) => failure(StatusCode::FORBIDDEN, error),
        };
    }
    // A body upload may outlive a revocation.
    if !authorized(&core, &token) {
        return failure(StatusCode::UNAUTHORIZED, "设备授权已撤销");
    }
    if path == "/remote/logout" {
        core.auth
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .logout(&token);
        let mut reply = json_response(StatusCode::OK, json!({"status":"ok"}));
        reply.headers_mut().insert(
            "set-cookie",
            "codey_remote=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"
                .parse()
                .unwrap(),
        );
        return reply;
    }
    let result =
        match path.as_str() {
            "/remote/threads" => {
                let search = args["search"].as_str().unwrap_or_default().to_string();
                let archived = args["archived"].as_bool().unwrap_or(false);
                tokio::task::spawn_blocking(move || {
                    super::store::threads(crate::codex_config::codex_home(), &search, archived)
                })
                .await
                .map_err(|_| "读取会话任务失败".to_string())
                .and_then(std::convert::identity)
            }
            "/remote/projects" => tokio::task::spawn_blocking(|| {
                super::store::projects(crate::codex_config::codex_home())
            })
            .await
            .map_err(|_| "读取项目任务失败".to_string())
            .and_then(std::convert::identity),
            "/remote/models" => {
                let Some(state) = core.state.upgrade() else {
                    return failure(StatusCode::SERVICE_UNAVAILABLE, "Codey 正在退出");
                };
                Ok(state
                    .bridge_request("/codex-model-catalog".into(), json!({}))
                    .await)
            }
            "/remote/action" | "/remote/create" => action_once(&core, &token, &path, &args).await,
            _ if path.starts_with("/api/") => {
                let command = path.trim_start_matches("/api/");
                if !panel_command(command) {
                    return failure(StatusCode::FORBIDDEN, "此操作仅可在电脑端使用");
                }
                let Some(state) = core.state.upgrade() else {
                    return failure(StatusCode::SERVICE_UNAVAILABLE, "Codey 正在退出");
                };
                let args = {
                    let config = state.config.read().await;
                    restore_panel_headers(&config, command, args)
                };
                let result = crate::commands::invoke_api(&state, command, args).await;
                Ok(redact_panel_config(result))
            }
            _ => return failure(StatusCode::NOT_FOUND, "远程接口不存在"),
        };
    match result {
        Ok(value) => json_response(StatusCode::OK, value),
        Err(error) => failure(StatusCode::BAD_REQUEST, &error),
    }
}

fn event_stream(
    core: Arc<Core>,
    token: String,
    thread: String,
    request: &mut Request<Incoming>,
) -> Reply {
    let headers = request.headers();
    let key = headers
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    use base64::Engine;
    if headers
        .get("upgrade")
        .is_none_or(|v| !v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        || headers
            .get("connection")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| {
                !v.split(',')
                    .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
            })
        || headers
            .get("sec-websocket-version")
            .is_none_or(|v| v != "13")
        || !base64::engine::general_purpose::STANDARD
            .decode(key)
            .is_ok_and(|v| v.len() == 16)
    {
        return failure(StatusCode::BAD_REQUEST, "需要有效的 WebSocket 连接");
    }
    let Ok(permit) = Arc::clone(&core.streams).try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "实时连接已达上限");
    };
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    let upgrade = hyper::upgrade::on(request);
    tokio::spawn(async move {
        let _permit = permit;
        let mut shutdown = core.shutdown.subscribe();
        let Ok(Ok(upgraded)) = tokio::time::timeout(Duration::from_secs(10), upgrade).await else {
            return;
        };
        let mut socket = WebSocketStream::from_raw_socket(
            TokioIo::new(upgraded),
            Role::Server,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(1024))
                    .max_frame_size(Some(1024)),
            ),
        )
        .await;
        let result = async {
            if *shutdown.borrow() || !authorized(&core,&token) { return Err("设备授权已撤销".to_string()); }
            let mut desktop = Desktop::follow(&thread).await?;
            let mut changed = true;
            let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
            let mut ticks = 0u8;
            loop {
                if *shutdown.borrow() || !authorized(&core,&token) { return Err("设备授权已撤销".to_string()); }
                if changed {
                    let value = json!({"type":"state","state":super::protocol::view(&desktop.state)});
                    tokio::time::timeout(Duration::from_secs(10),socket.send(Message::Text(value.to_string().into()))).await
                        .map_err(|_|"手机连接超时")?.map_err(|_|"手机连接已断开")?;
                }
                changed = false;
                tokio::select! {
                    _ = shutdown.changed() => return Ok::<_,String>(()),
                    _ = heartbeat.tick() => {
                        ticks = (ticks + 1) % 15;
                        if ticks == 0 {
                            tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Ping(Bytes::new()))).await
                                .map_err(|_|"手机连接超时")?.map_err(|_|"手机连接已断开")?;
                        }
                    },
                    result = desktop.update() => { changed = result?; },
                    message = socket.next() => match message {
                        Some(Ok(Message::Ping(bytes))) => { socket.send(Message::Pong(bytes)).await.map_err(|_|"手机连接已断开")?; },
                        Some(Ok(Message::Pong(_))) => {},
                        _ => return Ok(()),
                    }
                }
            }
        }.await;
        if let Err(error) = result {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                socket.send(Message::Text(
                    json!({"type":"disconnected","message":error})
                        .to_string()
                        .into(),
                )),
            )
            .await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), socket.close(None)).await;
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-accept", accept)
        .body(Full::new(Bytes::new()).boxed_unsync())
        .unwrap()
}

async fn action_once(core: &Core, token: &str, path: &str, args: &Value) -> Result<Value, String> {
    let id = args["requestId"].as_str().ok_or("操作缺少请求标识")?;
    super::desktop::validate_thread(id)?;
    let digest: [u8; 32] = Sha256::digest(format!("{path}\n{args}").as_bytes()).into();
    let mut ledger = core.actions.lock().await;
    if !authorized(core, token) {
        return Err("设备授权已撤销".into());
    }
    if let Some((previous, result)) = ledger.get(id) {
        if previous != &digest {
            return Err("同一操作标识不能用于不同内容".into());
        }
        return Ok(result.clone());
    }
    if ledger.len() >= 4096 {
        return Err("本次远程会话操作记录已满，请在电脑端重启远程服务".into());
    }
    ledger.insert(
        id.to_string(),
        (
            digest,
            json!({"status":"failed","message":super::desktop::UNCERTAIN}),
        ),
    );
    let result = if path == "/remote/create" {
        let state = core.state.upgrade().ok_or("Codey 正在退出")?;
        let app_path = state.config.read().await.codex_app_path.clone();
        super::create::create(
            crate::codex_config::codex_home().to_path_buf(),
            app_path,
            args["projectId"].as_str().unwrap_or_default().to_string(),
            args["title"].as_str().unwrap_or_default().to_string(),
        )
        .await
    } else {
        perform_action(core, args).await
    };
    let value = result.unwrap_or_else(|error| json!({"status":"failed","message":error}));
    ledger.insert(id.to_string(), (digest, value.clone()));
    Ok(value)
}

async fn perform_action(core: &Core, args: &Value) -> Result<Value, String> {
    let thread = args["threadId"].as_str().ok_or("缺少会话标识")?;
    super::desktop::validate_thread(thread)?;
    let action = args["action"].as_str().ok_or("缺少会话操作")?;
    if action == "open" {
        if Desktop::follow(thread).await.is_err() {
            super::create::open(thread).await?;
        }
        return Ok(json!({"status":"ok"}));
    }
    let mut desktop = Desktop::follow(thread).await?;
    if action == "settings" && (args.get("model").is_some() || args.get("effort").is_some()) {
        let state = core.state.upgrade().ok_or("Codey 正在退出")?;
        let catalog = state
            .bridge_request("/codex-model-catalog".into(), json!({}))
            .await;
        validate_model_settings(&catalog, &desktop.state, args)?;
    }
    desktop.action(action, args).await
}

fn validate_model_settings(catalog: &Value, desktop: &Value, args: &Value) -> Result<(), String> {
    if catalog["status"] != "ok" || catalog["clear_models"] == true {
        return Err("模型目录暂不可用，请刷新模型列表".into());
    }
    let model = args.get("model").unwrap_or_else(|| {
        desktop["latestThreadSettings"]
            .get("model")
            .unwrap_or(&desktop["latestModel"])
    });
    if !catalog["models"]
        .as_array()
        .is_some_and(|models| models.contains(model))
    {
        return Err("该模型已不可用，请刷新模型列表后重新选择".into());
    }
    if let Some(effort) = args.get("effort") {
        let supported = catalog["model_metadata"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["model"] == *model)
            .and_then(|entry| entry["supported_reasoning_efforts"].as_array())
            .is_some_and(|values| values.contains(effort));
        if !supported {
            return Err("该模型不支持此思考程度，请刷新模型列表".into());
        }
    }
    Ok(())
}

fn redact_panel_config(mut value: Value) -> Value {
    if let Some(config) = value.get_mut("config").filter(|config| config.is_object()) {
        if let Some(profiles) = config.get_mut("profiles").and_then(Value::as_array_mut) {
            for profile in profiles {
                if !profile.is_object() {
                    continue;
                }
                profile["apiKey"] = json!("");
                // Request headers may contain provider credentials too. Keep
                // them on the host when remote users save the redacted form.
                profile["modelRequestHeaders"] = json!({});
            }
        }
        if let Some(prompt) = config
            .get_mut("promptOptimization")
            .filter(|value| value.is_object())
        {
            prompt["apiKey"] = json!("");
        }
        if let Some(channels) = config
            .get_mut("webhook")
            .and_then(|value| value.get_mut("channels"))
            .and_then(Value::as_array_mut)
        {
            for channel in channels {
                if !channel.is_object() {
                    continue;
                }
                channel["botToken"] = json!("");
                channel["url"] = json!("");
            }
        }
    }
    value
}

fn restore_panel_headers(
    config: &crate::config::CodeyConfig,
    command: &str,
    mut args: Value,
) -> Value {
    if command != "save_codey_config" {
        return args;
    }
    if let Some(profiles) = args
        .get_mut("config")
        .and_then(|config| config.get_mut("profiles"))
        .and_then(Value::as_array_mut)
    {
        for profile in profiles {
            if !profile.is_object() {
                continue;
            }
            if profile
                .get("modelRequestHeaders")
                .is_none_or(|v| v.as_object().is_none_or(|headers| headers.is_empty()))
                && let Some(saved) = config
                    .profiles
                    .iter()
                    .find(|saved| Some(saved.id.as_str()) == profile["id"].as_str())
            {
                profile["modelRequestHeaders"] = serde_json::to_value(&saved.model_request_headers)
                    .unwrap_or_else(|_| json!({}));
            }
        }
    }
    args
}

fn panel_command(command: &str) -> bool {
    matches!(
        command,
        "load_codey_config"
            | "save_codey_config"
            | "sync_current_provider"
            | "set_route_enabled"
            | "reorder_route_models"
            | "delete_route"
            | "fetch_route_models"
            | "save_selected_models"
            | "save_default_model"
            | "save_official_route_models"
            | "runtime_status"
            | "query_route_request_logs"
            | "query_route_request_log_stats"
            | "query_route_request_log_quota_usage"
            | "query_route_request_log_models"
            | "query_official_account_usage"
            | "store_official_account_usage"
            | "list_official_accounts"
            | "refresh_official_account_routes"
            | "start_official_account_login"
            | "poll_official_account_login"
            | "cancel_official_account_login"
            | "import_current_codex_login"
            | "import_official_account_credential"
            | "set_default_official_account"
            | "remove_official_account"
            | "save_official_account_route_settings"
            | "clear_route_request_logs"
            | "restart_codey"
            | "clear_diagnostic_storage"
            | "repair_codex_overlays"
            | "test_notification_channel"
            | "start_wechat_claw_login"
            | "poll_wechat_claw_login"
            | "optimize_prompt"
            | "test_prompt_optimization"
            | "fetch_prompt_optimization_models"
            | "check_for_updates"
            | "get_device_machine_no"
            | "download_update"
            | "install_downloaded_update"
            | "update_install_report"
            | "plugin_marketplace_status"
            | "repair_plugin_marketplace"
            | "prepare_computer_use"
            | "repair_main_process_injection"
            | "repair_codex_config"
            | "list_codey_plugins"
            | "get_codey_plugin_config_file"
            | "inspect_codey_plugin"
            | "install_codey_plugin"
            | "set_codey_plugin_enabled"
            | "save_codey_plugin_config_file"
            | "uninstall_codey_plugin"
            | "clear_codey_plugin_logs"
            | "invoke_codey_plugin"
            | "codex_extensions"
    )
}

#[cfg(test)]
mod tests;
