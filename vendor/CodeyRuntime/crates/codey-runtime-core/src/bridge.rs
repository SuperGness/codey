use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, bail};
use futures_util::stream::FuturesUnordered;
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

pub const BRIDGE_BINDING_NAME: &str = "codexSessionDeleteV2";
const CDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CDP_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const BRIDGE_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const BRIDGE_MAX_CONCURRENT_READ_HANDLERS: usize = 8;
const BRIDGE_MAX_PENDING_CALLS: usize = 256;

pub type BridgeHandler = Arc<
    dyn Fn(String, Value) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>
        + Send
        + Sync,
>;

#[must_use = "dropping the handle closes the CDP bridge message pump"]
#[derive(Debug)]
pub struct BridgePumpHandle {
    task: Option<tokio::task::JoinHandle<()>>,
    closing: Arc<AtomicBool>,
    shutdown: Arc<Notify>,
}

impl BridgePumpHandle {
    pub fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }

    pub async fn close(mut self) {
        self.closing.store(true, Ordering::Release);
        self.shutdown.notify_one();
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(BRIDGE_CLOSE_TIMEOUT, &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for BridgePumpHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            self.closing.store(true, Ordering::Release);
            self.shutdown.notify_one();
            task.abort();
        }
    }
}

/// 同一 target 上已注册的持久文档脚本标识（按 WebSocket 地址区分目标）。重复
/// 装桥时先注销上一批再注册，否则每次重试或看门狗重建都会让脚本在后续导航里
/// 多执行一遍。标识只在同一 target 上有效；目标销毁后的残项随下次装桥清掉。
static DOCUMENT_SCRIPT_REGISTRATIONS: OnceLock<Mutex<HashMap<String, Vec<String>>>> =
    OnceLock::new();

fn document_script_registrations() -> &'static Mutex<HashMap<String, Vec<String>>> {
    DOCUMENT_SCRIPT_REGISTRATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// addScriptToEvaluateOnNewDocument 的响应带 `result.identifier`；注册一条记
/// 一条，装桥中途失败留下的残项也能被下次装桥完整注销。
fn remember_document_script(websocket_url: &str, response: &Value) {
    let Some(identifier) = response
        .get("result")
        .and_then(|result| result.get("identifier"))
        .and_then(Value::as_str)
    else {
        return;
    };
    document_script_registrations()
        .lock()
        .expect("document script registry poisoned")
        .entry(websocket_url.to_string())
        .or_default()
        .push(identifier.to_string());
}

static NEXT_MESSAGE_ID: AtomicU64 = AtomicU64::new(100);

pub fn build_bridge_script(binding_name: &str) -> String {
    build_bridge_script_for_session(binding_name, "")
}

fn build_bridge_script_for_session(binding_name: &str, session_token: &str) -> String {
    let session_token = serde_json::to_string(session_token)
        .expect("serializing a bridge session token cannot fail");
    format!(
        r#"
(() => {{
  const bridgeSession = {session_token};
  // 重新注入会替换回调表，先把上一代表里仍在等待的调用全部置为失败，
  // 否则它们的 Promise 永远挂起，调用方（如会话时间戳刷新）会被一次性卡死。
  const previousCallbacks = window.__codexSessionDeleteCallbacks;
  if (previousCallbacks && typeof previousCallbacks.forEach === "function") {{
    previousCallbacks.forEach((callback) => {{
      try {{
        if (callback?.timeout) window.clearTimeout(callback.timeout);
        callback?.resolve?.({{
          status: "failed",
          code: "bridge_reinstalled",
          message: "Codey 桥接已重新注入，请重试",
        }});
      }} catch {{}}
    }});
  }}
  window.__codexSessionDeleteCallbacks = new Map();
  window.__codexSessionDeleteSeq = 0;
  const takeCallback = (id) => {{
    const callback = window.__codexSessionDeleteCallbacks.get(id);
    if (!callback) return null;
    window.__codexSessionDeleteCallbacks.delete(id);
    if (callback.timeout) window.clearTimeout(callback.timeout);
    return callback;
  }};
  window.__codexSessionDeleteResolve = (id, result) => {{
    const callback = takeCallback(id);
    if (!callback) return;
    callback.resolve(result);
  }};
  window.__codexSessionDeleteReject = (id, message) => {{
    const callback = takeCallback(id);
    if (!callback) return;
    callback.resolve({{ status: "failed", code: "bridge_request_failed", message }});
  }};
  window.__codexSessionDeleteBridge = (path, payload, options = {{}}) => new Promise((resolve) => {{
    const id = String(++window.__codexSessionDeleteSeq);
    const configuredTimeout = Number(options?.timeoutMs);
    const timeoutMs = Number.isFinite(configuredTimeout)
      ? Math.max(250, Math.min(configuredTimeout, 60_000))
      : 0;
    const callback = {{ resolve, timeout: 0 }};
    window.__codexSessionDeleteCallbacks.set(id, callback);
    if (timeoutMs > 0) {{
      callback.timeout = window.setTimeout(() => {{
        const expired = takeCallback(id);
        if (!expired) return;
        expired.resolve({{
          status: "failed",
          code: "bridge_timeout",
          message: "Codey 后端响应超时",
          timeout: true,
        }});
      }}, timeoutMs);
    }}
    try {{
      window.{binding_name}(JSON.stringify({{ id, path, payload, bridgeSession }}));
    }} catch (error) {{
      const failed = takeCallback(id);
      if (!failed) return;
      failed.resolve({{
        status: "failed",
        code: "bridge_unavailable",
        message: error instanceof Error ? error.message : String(error),
      }});
    }}
  }});
}})();
"#
    )
}

pub fn bridge_health_check_script() -> &'static str {
    // Tri-state probe: "busy" means the bridge is installed but the page could
    // not round-trip within the in-page budget, which must not be confused
    // with a missing bridge — reinjecting into a busy renderer adds more work
    // to an already stalled page.
    r#"
(() => {
  const bridge = window.__codexSessionDeleteBridge;
  if (typeof bridge !== "function") return "missing";
  try {
    return Promise.race([
      Promise.resolve(bridge("/backend/health", {})).then((result) => (
        !!result && result.status === "ok" ? "healthy" : "unhealthy"
      )),
      new Promise((resolve) => setTimeout(() => resolve("busy"), 2000)),
    ]);
  } catch (error) {
    return "missing";
  }
})()
"#
}

pub async fn evaluate_script(websocket_url: &str, script: &str) -> anyhow::Result<Value> {
    evaluate_script_with_await_promise(websocket_url, script, false).await
}

pub async fn evaluate_script_with_await_promise(
    websocket_url: &str,
    script: &str,
    await_promise: bool,
) -> anyhow::Result<Value> {
    evaluate_script_with_await_promise_timeout(
        websocket_url,
        script,
        await_promise,
        CDP_COMMAND_TIMEOUT,
    )
    .await
}

pub async fn evaluate_script_with_await_promise_timeout(
    websocket_url: &str,
    script: &str,
    await_promise: bool,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let socket = connect_cdp_websocket(websocket_url).await?;
    let mut session = CdpSession::new(socket);
    let response = session
        .send_command_with_timeout(
            1,
            "Runtime.evaluate",
            runtime_evaluate_params_with_await_promise(script, await_promise),
            timeout,
        )
        .await?;
    ensure_runtime_evaluate_succeeded(response)
}

pub async fn install_bridge(
    websocket_url: &str,
    binding_name: &str,
    handler: BridgeHandler,
    new_document_scripts: &[String],
) -> anyhow::Result<BridgePumpHandle> {
    let socket = connect_cdp_websocket(websocket_url).await?;
    let session_token = format!("bridge-{}", next_message_id());
    let mut session = CdpSession::new(socket)
        .with_handler(handler)
        .with_binding_session(session_token.clone());

    session.send_command(1, "Runtime.enable", json!({})).await?;
    session
        .send_command(2, "Runtime.removeBinding", json!({ "name": binding_name }))
        .await?;
    session
        .send_command(3, "Runtime.addBinding", json!({ "name": binding_name }))
        .await?;

    // 重复装桥先注销同一 target 上的旧持久脚本：注册标识没有枚举接口，只能由
    // 上一次装桥记下。注销失败不阻断安装，残项随下次装桥再清。
    let stale_registrations = document_script_registrations()
        .lock()
        .expect("document script registry poisoned")
        .remove(websocket_url)
        .unwrap_or_default();
    for identifier in stale_registrations {
        let _ = session
            .send_command(
                next_message_id(),
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({ "identifier": identifier }),
            )
            .await;
    }

    let bridge_script = build_bridge_script_for_session(binding_name, &session_token);
    let response = session
        .send_command(
            4,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": bridge_script }),
        )
        .await?;
    remember_document_script(websocket_url, &response);
    let response = session
        .send_command(
            5,
            "Runtime.evaluate",
            runtime_evaluate_params(&bridge_script),
        )
        .await?;
    ensure_runtime_evaluate_succeeded(response)?;

    for script in new_document_scripts {
        let message_id = next_message_id();
        let response = session
            .send_command(
                message_id,
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": script }),
            )
            .await?;
        remember_document_script(websocket_url, &response);
        let message_id = next_message_id();
        let response = session
            .send_command(
                message_id,
                "Runtime.evaluate",
                runtime_evaluate_params(script),
            )
            .await?;
        ensure_runtime_evaluate_succeeded(response)?;
    }

    let closing = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(Notify::new());
    let task_closing = Arc::clone(&closing);
    let task_shutdown = Arc::clone(&shutdown);
    let task = tokio::spawn(async move {
        if let Err(error) = run_bridge_pump(session, task_closing, task_shutdown).await {
            let _ = crate::diagnostic_log::append_diagnostic_log(
                "bridge.pump_failed",
                json!({ "message": error.to_string() }),
            );
        }
    });

    Ok(BridgePumpHandle {
        task: Some(task),
        closing,
        shutdown,
    })
}

pub fn runtime_evaluate_params(script: &str) -> Value {
    runtime_evaluate_params_with_await_promise(script, false)
}

pub fn runtime_evaluate_params_with_await_promise(script: &str, await_promise: bool) -> Value {
    json!({
        "expression": script,
        "awaitPromise": await_promise,
        "allowUnsafeEvalBlockedByCSP": true,
    })
}

pub fn resolve_bridge_expression(request_id: &str, result: &Value) -> anyhow::Result<String> {
    Ok(format!(
        "window.__codexSessionDeleteResolve({}, {})",
        serde_json::to_string(request_id)?,
        serde_json::to_string(result)?,
    ))
}

pub fn reject_bridge_expression(request_id: &str, message: &str) -> anyhow::Result<String> {
    Ok(format!(
        "window.__codexSessionDeleteReject({}, {})",
        serde_json::to_string(request_id)?,
        serde_json::to_string(message)?,
    ))
}

type ConnectedCdpSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_cdp_websocket(websocket_url: &str) -> anyhow::Result<ConnectedCdpSocket> {
    let candidates = cdp_websocket_connect_candidates(websocket_url);
    // 候选共享原来的连接总预算，避免回退把单轮代价翻倍后吃掉启动注入窗口。
    let attempt_timeout = cdp_connect_attempt_timeout(candidates.len());
    let mut last_error = None;
    for candidate in &candidates {
        // The TLS-capable handshake future is large even for loopback ws://.
        // Keep it out of every enclosing startup future: their nested poll
        // frames can otherwise exhaust the Windows main thread's 1 MiB stack.
        let connect = Box::pin(connect_async(candidate.as_str()));
        match tokio::time::timeout(attempt_timeout, connect).await {
            Ok(Ok((socket, _))) => return Ok(socket),
            Ok(Err(error)) => {
                last_error =
                    Some(anyhow::Error::new(error).context("failed to connect CDP websocket"));
            }
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "timed out connecting CDP websocket after {}s",
                    CDP_CONNECT_TIMEOUT.as_secs()
                ));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("CDP websocket address is empty")))
}

fn cdp_connect_attempt_timeout(candidate_count: usize) -> Duration {
    CDP_CONNECT_TIMEOUT / candidate_count.max(1) as u32
}

/// Chromium 的 `/json` 会把请求用的 Host 原样写进 `webSocketDebuggerUrl`，而
/// Codey 枚举 CDP 时优先使用 `localhost`（Chromium 150+ 拒绝 IP 字面量 Host），
/// 于是拿到的地址通常是 `ws://localhost:<port>/devtools/page/<id>`。
/// `connect_async` 对一个主机名只按解析顺序逐个尝试、每个地址没有独立超时：若
/// 排在前面的是被安全软件静默丢弃的回环 IPv6，它会一直挂到预算耗尽而轮不到
/// `127.0.0.1`，表现就是 HTTP 列表可达但 WebSocket 连不上（HTTP 侧已显式枚举
/// 三个回环地址，因此不受影响）。这里把回环别名展开成显式候选，先用回环 IPv4
/// 直连，再退回原始主机名。
fn cdp_websocket_connect_candidates(websocket_url: &str) -> Vec<String> {
    let trimmed = websocket_url.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return vec![trimmed.to_string()];
    };
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = split_authority_host_port(authority);
    let mut candidates = Vec::with_capacity(2);
    if is_loopback_alias(host) {
        let port = port.map_or(String::new(), |port| format!(":{port}"));
        candidates.push(format!("{scheme}://127.0.0.1{port}{path}"));
    }
    candidates.push(trimmed.to_string());
    candidates.dedup();
    candidates
}

fn is_loopback_alias(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "[::1]" || host == "::1"
}

fn split_authority_host_port(authority: &str) -> (&str, Option<&str>) {
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) => (
                &authority[..host.len() + 2],
                tail.strip_prefix(':').filter(|port| !port.is_empty()),
            ),
            None => (authority, None),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => (host, Some(port)),
        _ => (authority, None),
    }
}

struct BridgeCall {
    request_id: String,
    path: String,
    payload: Value,
}

enum BridgeCompletion {
    Resolve { request_id: String, result: Value },
    Reject { request_id: String, message: String },
}

enum ParsedBridgeDispatch {
    Call(BridgeCall),
    Completion(BridgeCompletion),
}

type BridgeCallFuture = Pin<Box<dyn Future<Output = BridgeCompletion> + Send>>;

async fn run_bridge_pump(
    session: CdpSession<ConnectedCdpSocket>,
    closing: Arc<AtomicBool>,
    shutdown: Arc<Notify>,
) -> anyhow::Result<()> {
    let CdpSession {
        socket,
        mut binding_calls,
        handler,
        binding_session,
        ..
    } = session;
    let Some(handler) = handler else {
        return Ok(());
    };
    let (mut writer, mut reader) = socket.split();
    let mut pending_concurrent = VecDeque::<BridgeCall>::new();
    let mut pending_serial = VecDeque::<BridgeCall>::new();
    let mut ready = VecDeque::<BridgeCompletion>::new();
    let mut concurrent = FuturesUnordered::<BridgeCallFuture>::new();
    let mut serial = FuturesUnordered::<BridgeCallFuture>::new();
    let mut git_work = FuturesUnordered::<BridgeCallFuture>::new();
    let mut stream_open = true;

    while let Some(message) = binding_calls.pop_front() {
        queue_bridge_dispatch(
            parse_bridge_binding_call(message, binding_session.as_deref()),
            &mut pending_concurrent,
            &mut pending_serial,
            &mut ready,
        );
    }

    loop {
        while let Some(index) = pending_concurrent.iter().position(|call| {
            if bridge_path_is_git_work(&call.path) {
                git_work.len() < 4
            } else {
                concurrent.len() < BRIDGE_MAX_CONCURRENT_READ_HANDLERS
            }
        }) {
            let call = pending_concurrent.remove(index).unwrap();
            let lane = if bridge_path_is_git_work(&call.path) {
                &mut git_work
            } else {
                &mut concurrent
            };
            lane.push(execute_bridge_call(Arc::clone(&handler), call));
        }
        if serial.is_empty()
            && let Some(call) = pending_serial.pop_front()
        {
            serial.push(execute_bridge_call(Arc::clone(&handler), call));
        }

        if let Some(completion) = ready.pop_front() {
            send_bridge_completion(&mut writer, completion).await?;
            continue;
        }
        if closing.load(Ordering::Acquire) {
            break;
        }
        if !stream_open
            && concurrent.is_empty()
            && serial.is_empty()
            && git_work.is_empty()
            && pending_concurrent.is_empty()
            && pending_serial.is_empty()
        {
            break;
        }

        let pending_count = pending_concurrent
            .len()
            .saturating_add(pending_serial.len());
        let can_read = stream_open && pending_count < BRIDGE_MAX_PENDING_CALLS;
        tokio::select! {
            _ = shutdown.notified() => {
                if closing.load(Ordering::Acquire) {
                    break;
                }
            }
            completion = concurrent.next(), if !concurrent.is_empty() => {
                if let Some(completion) = completion {
                    ready.push_back(completion);
                }
            }
            completion = git_work.next(), if !git_work.is_empty() => {
                if let Some(completion) = completion {
                    ready.push_back(completion);
                }
            }
            completion = serial.next(), if !serial.is_empty() => {
                if let Some(completion) = completion {
                    ready.push_back(completion);
                }
            }
            message = reader.next(), if can_read => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let value = serde_json::from_str::<Value>(&text)
                            .context("failed to parse CDP message")?;
                        if value.get("method").and_then(Value::as_str)
                            == Some("Runtime.bindingCalled")
                        {
                            queue_bridge_dispatch(
                                parse_bridge_binding_call(value, binding_session.as_deref()),
                                &mut pending_concurrent,
                                &mut pending_serial,
                                &mut ready,
                            );
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        stream_open = false;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        return Err(error).context("failed to read CDP websocket message");
                    }
                }
            }
        }
    }

    let _ = writer.close().await;
    Ok(())
}

fn parse_bridge_binding_call(
    message: Value,
    binding_session: Option<&str>,
) -> Option<ParsedBridgeDispatch> {
    let payload_text = message
        .get("params")
        .and_then(|params| params.get("payload"))
        .and_then(Value::as_str)?;
    let parsed: Value = match serde_json::from_str(payload_text) {
        Ok(parsed) => parsed,
        Err(error) => {
            let request_id = extract_string_field(payload_text, "id")?;
            return Some(ParsedBridgeDispatch::Completion(BridgeCompletion::Reject {
                request_id,
                message: format!("failed to parse bridge payload: {error}"),
            }));
        }
    };
    if let (Some(expected), Some(actual)) = (
        binding_session,
        parsed.get("bridgeSession").and_then(Value::as_str),
    ) && actual != expected
    {
        return None;
    }
    let request_id = parsed.get("id").and_then(Value::as_str)?.to_string();
    let path = parsed
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let payload = parsed.get("payload").cloned().unwrap_or_else(|| json!({}));
    Some(ParsedBridgeDispatch::Call(BridgeCall {
        request_id,
        path,
        payload,
    }))
}

fn queue_bridge_dispatch(
    dispatch: Option<ParsedBridgeDispatch>,
    pending_concurrent: &mut VecDeque<BridgeCall>,
    pending_serial: &mut VecDeque<BridgeCall>,
    ready: &mut VecDeque<BridgeCompletion>,
) {
    match dispatch {
        Some(ParsedBridgeDispatch::Call(call)) if bridge_path_can_run_concurrently(&call.path) => {
            pending_concurrent.push_back(call);
        }
        Some(ParsedBridgeDispatch::Call(call)) => pending_serial.push_back(call),
        Some(ParsedBridgeDispatch::Completion(completion)) => ready.push_back(completion),
        None => {}
    }
}

fn bridge_path_is_git_work(path: &str) -> bool {
    matches!(
        path,
        "/api/conversation_git_preview"
            | "/api/conversation_git_push_preview"
            | "/api/conversation_git_execute"
    )
}

fn bridge_path_can_run_concurrently(path: &str) -> bool {
    bridge_path_is_git_work(path)
        || matches!(
            path,
            "/settings/get"
                | "/codex-model-catalog"
                | "/backend/status"
                | "/backend/health"
                | "/account/usage"
                | "/session/completion-state"
                | "/api/check_for_updates"
                | "/session/wake-watcher"
                | "/plugins/list"
                | "/api/conversation_git_status"
                | "/api/conversation_git_cancel"
        )
}

fn execute_bridge_call(handler: BridgeHandler, call: BridgeCall) -> BridgeCallFuture {
    Box::pin(async move {
        match handler(call.path, call.payload).await {
            Ok(result) => BridgeCompletion::Resolve {
                request_id: call.request_id,
                result,
            },
            Err(error) => BridgeCompletion::Reject {
                request_id: call.request_id,
                message: error.to_string(),
            },
        }
    })
}

async fn send_bridge_completion<S>(
    writer: &mut S,
    completion: BridgeCompletion,
) -> anyhow::Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let (request_id, expression, failure_event) = match completion {
        BridgeCompletion::Resolve { request_id, result } => {
            let expression = resolve_bridge_expression(&request_id, &result)?;
            (request_id, expression, "bridge.resolve_failed")
        }
        BridgeCompletion::Reject {
            request_id,
            message,
        } => {
            let expression = reject_bridge_expression(&request_id, &message)?;
            (request_id, expression, "bridge.reject_failed")
        }
    };
    let message_id = next_message_id();
    let sent = writer
        .send(Message::Text(
            json!({
                "id": message_id,
                "method": "Runtime.evaluate",
                "params": runtime_evaluate_params(&expression),
            })
            .to_string()
            .into(),
        ))
        .await;
    if let Err(error) = &sent {
        let _ = crate::diagnostic_log::append_diagnostic_log(
            failure_event,
            json!({
                "request_id": request_id,
                "message_id": message_id,
                "message": error.to_string(),
            }),
        );
    }
    sent.with_context(|| format!("failed to send bridge response for request {request_id}"))
}

struct CdpSession<S> {
    socket: S,
    responses: HashMap<u64, Value>,
    binding_calls: VecDeque<Value>,
    handler: Option<BridgeHandler>,
    binding_session: Option<String>,
}

impl<S> CdpSession<S>
where
    S: SinkExt<Message>
        + StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin
        + Send,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    fn new(socket: S) -> Self {
        Self {
            socket,
            responses: HashMap::new(),
            binding_calls: VecDeque::new(),
            handler: None,
            binding_session: None,
        }
    }

    fn with_handler(mut self, handler: BridgeHandler) -> Self {
        self.handler = Some(handler);
        self
    }

    fn with_binding_session(mut self, session_token: String) -> Self {
        self.binding_session = Some(session_token);
        self
    }

    async fn send_command(
        &mut self,
        message_id: u64,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        self.send_command_with_timeout(message_id, method, params, CDP_COMMAND_TIMEOUT)
            .await
    }

    async fn send_command_with_timeout(
        &mut self,
        message_id: u64,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        self.socket
            .send(Message::Text(
                json!({
                    "id": message_id,
                    "method": method,
                    "params": params,
                })
                .to_string()
                .into(),
            ))
            .await
            .with_context(|| format!("failed to send CDP command {method} id {message_id}"))?;

        tokio::time::timeout(timeout, self.wait_for_id(message_id, method.to_string()))
            .await
            .with_context(|| {
                format!(
                    "timed out waiting for CDP command {method} id {message_id} response after {}s",
                    timeout.as_secs()
                )
            })?
    }

    async fn wait_for_id(&mut self, message_id: u64, method: String) -> anyhow::Result<Value> {
        loop {
            if let Some(response) = self.responses.remove(&message_id) {
                return command_result(response, &method, message_id);
            }

            let Some(message) = self.next_message().await? else {
                bail!("CDP websocket closed before response for {method} id {message_id}");
            };

            if let Some(response_id) = message.get("id").and_then(Value::as_u64) {
                if response_id == message_id {
                    return command_result(message, &method, message_id);
                }
                self.responses.insert(response_id, message);
            }
        }
    }

    async fn next_message(&mut self) -> anyhow::Result<Option<Value>> {
        let Some(message) = self.socket.next().await else {
            return Ok(None);
        };
        let message = message.context("failed to read CDP websocket message")?;
        let Message::Text(text) = message else {
            return Ok(Some(json!({})));
        };
        let value: Value = serde_json::from_str(&text).context("failed to parse CDP message")?;

        if value.get("method").and_then(Value::as_str) == Some("Runtime.bindingCalled") {
            self.binding_calls.push_back(value);
            return Ok(Some(json!({})));
        }

        Ok(Some(value))
    }
}

fn command_result(response: Value, method: &str, message_id: u64) -> anyhow::Result<Value> {
    if let Some(error) = response.get("error") {
        bail!("CDP command {method} id {message_id} failed: {error}");
    }
    Ok(response)
}

fn ensure_runtime_evaluate_succeeded(response: Value) -> anyhow::Result<Value> {
    if let Some(exception) = response
        .get("result")
        .and_then(|result| result.get("exceptionDetails"))
    {
        bail!("Runtime.evaluate raised an exception: {exception}");
    }
    Ok(response)
}

fn extract_string_field(input: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let mut index = input.find(&needle)? + needle.len();
    let bytes = input.as_bytes();

    while matches!(bytes.get(index), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        index += 1;
    }
    if bytes.get(index) != Some(&b':') {
        return None;
    }
    index += 1;
    while matches!(bytes.get(index), Some(b' ' | b'\n' | b'\r' | b'\t')) {
        index += 1;
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    index += 1;

    let mut output = String::new();
    let mut escaped = false;
    for ch in input[index..].chars() {
        if escaped {
            output.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(output),
            _ => output.push(ch),
        }
    }

    None
}

fn next_message_id() -> u64 {
    NEXT_MESSAGE_ID.fetch_add(1, Ordering::Relaxed) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_connect_future_keeps_handshake_off_the_startup_stack() {
        // Construct, but do not poll: this must not open a socket or need a runtime.
        let connect = connect_cdp_websocket("ws://127.0.0.1:9229/devtools/page/test");
        let bytes = std::mem::size_of_val(&connect);
        assert!(
            bytes <= 1024,
            "CDP connect future embeds handshake state: {bytes} bytes"
        );
    }

    #[test]
    fn loopback_websocket_aliases_expand_to_ipv4_first() {
        assert_eq!(
            cdp_websocket_connect_candidates("ws://localhost:9229/devtools/page/ABC"),
            [
                "ws://127.0.0.1:9229/devtools/page/ABC",
                "ws://localhost:9229/devtools/page/ABC",
            ]
        );
        assert_eq!(
            cdp_websocket_connect_candidates("ws://[::1]:9229/devtools/page/ABC"),
            [
                "ws://127.0.0.1:9229/devtools/page/ABC",
                "ws://[::1]:9229/devtools/page/ABC",
            ]
        );
        assert_eq!(
            cdp_websocket_connect_candidates("ws://localhost/devtools/page/ABC"),
            [
                "ws://127.0.0.1/devtools/page/ABC",
                "ws://localhost/devtools/page/ABC",
            ]
        );
    }

    #[test]
    fn explicit_websocket_hosts_stay_single_candidate() {
        for url in [
            "ws://127.0.0.1:9229/devtools/page/ABC",
            "wss://codex.example/devtools/page/ABC",
        ] {
            let candidates = cdp_websocket_connect_candidates(url);
            assert_eq!(candidates, [url], "{url} must be tried as written");
        }
    }

    #[test]
    fn websocket_connect_candidates_share_one_connect_budget() {
        assert_eq!(cdp_connect_attempt_timeout(0), CDP_CONNECT_TIMEOUT);
        assert_eq!(cdp_connect_attempt_timeout(1), CDP_CONNECT_TIMEOUT);
        assert_eq!(cdp_connect_attempt_timeout(2), CDP_CONNECT_TIMEOUT / 2);
    }

    fn bridge_call(request_id: &str, path: &str) -> ParsedBridgeDispatch {
        ParsedBridgeDispatch::Call(BridgeCall {
            request_id: request_id.to_string(),
            path: path.to_string(),
            payload: json!({}),
        })
    }

    #[test]
    fn update_checks_use_the_concurrent_read_lane() {
        let mut pending_concurrent = VecDeque::new();
        let mut pending_serial = VecDeque::new();
        let mut ready = VecDeque::new();
        for (request_id, path) in [
            ("update-check", "/api/check_for_updates"),
            ("settings", "/settings/set"),
        ] {
            queue_bridge_dispatch(
                Some(bridge_call(request_id, path)),
                &mut pending_concurrent,
                &mut pending_serial,
                &mut ready,
            );
        }

        assert_eq!(
            pending_concurrent
                .iter()
                .map(|call| call.request_id.as_str())
                .collect::<Vec<_>>(),
            ["update-check"]
        );
        assert_eq!(
            pending_serial
                .iter()
                .map(|call| call.request_id.as_str())
                .collect::<Vec<_>>(),
            ["settings"]
        );
        assert!(ready.is_empty());
    }
}
