use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

    pub fn detach(mut self) {
        let _ = self.task.take();
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
    let socket = connect_cdp_websocket(websocket_url).await?;
    let mut session = CdpSession::new(socket);
    let response = session
        .send_command(
            1,
            "Runtime.evaluate",
            runtime_evaluate_params_with_await_promise(script, await_promise),
        )
        .await?;
    ensure_runtime_evaluate_succeeded(response)
}

pub async fn run_periodic_evaluations<F>(
    websocket_url: &str,
    period: Duration,
    mut next_expression: F,
) -> anyhow::Result<()>
where
    F: FnMut() -> anyhow::Result<Option<String>>,
{
    let socket = connect_cdp_websocket(websocket_url).await?;
    let mut session = CdpSession::new(socket);
    let mut interval = tokio::time::interval(period);
    loop {
        interval.tick().await;
        let Some(expression) = next_expression()? else {
            return Ok(());
        };
        let response = session
            .send_command(
                next_message_id(),
                "Runtime.evaluate",
                runtime_evaluate_params(&expression),
            )
            .await?;
        let response = ensure_runtime_evaluate_succeeded(response)?;
        if runtime_evaluate_result_is_false(&response) {
            bail!("periodic Runtime.evaluate reported unavailable capability");
        }
    }
}

pub async fn add_script_to_new_documents(
    websocket_url: &str,
    script: &str,
) -> anyhow::Result<Value> {
    let socket = connect_cdp_websocket(websocket_url).await?;
    let mut session = CdpSession::new(socket);
    session
        .send_command(
            1,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": script }),
        )
        .await
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

    let bridge_script = build_bridge_script_for_session(binding_name, &session_token);
    session
        .send_command(
            4,
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": bridge_script }),
        )
        .await?;
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
        session
            .send_command(
                message_id,
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": script }),
            )
            .await?;
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
    let (socket, _) = tokio::time::timeout(CDP_CONNECT_TIMEOUT, connect_async(websocket_url))
        .await
        .with_context(|| {
            format!(
                "timed out connecting CDP websocket after {}s",
                CDP_CONNECT_TIMEOUT.as_secs()
            )
        })?
        .context("failed to connect CDP websocket")?;

    Ok(socket)
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
        while concurrent.len() < BRIDGE_MAX_CONCURRENT_READ_HANDLERS {
            let Some(call) = pending_concurrent.pop_front() else {
                break;
            };
            concurrent.push(execute_bridge_call(Arc::clone(&handler), call));
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
        Some(ParsedBridgeDispatch::Call(call))
            if bridge_path_is_high_priority_serial(&call.path) =>
        {
            let insertion_index = pending_serial
                .iter()
                .position(|queued| !bridge_path_is_high_priority_serial(&queued.path))
                .unwrap_or(pending_serial.len());
            pending_serial.insert(insertion_index, call);
        }
        Some(ParsedBridgeDispatch::Call(call)) => pending_serial.push_back(call),
        Some(ParsedBridgeDispatch::Completion(completion)) => ready.push_back(completion),
        None => {}
    }
}

fn bridge_path_is_high_priority_serial(path: &str) -> bool {
    matches!(path, "/session/delete")
}

fn bridge_path_can_run_concurrently(path: &str) -> bool {
    matches!(
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

        tokio::time::timeout(
            CDP_COMMAND_TIMEOUT,
            self.wait_for_id(message_id, method.to_string()),
        )
        .await
        .with_context(|| {
            format!(
                "timed out waiting for CDP command {method} id {message_id} response after {}s",
                CDP_COMMAND_TIMEOUT.as_secs()
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

fn runtime_evaluate_result_is_false(response: &Value) -> bool {
    response
        .get("result")
        .and_then(|result| result.get("result"))
        .and_then(|result| result.get("value"))
        .is_some_and(|value| value == false)
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

    fn bridge_call(request_id: &str, path: &str) -> ParsedBridgeDispatch {
        ParsedBridgeDispatch::Call(BridgeCall {
            request_id: request_id.to_string(),
            path: path.to_string(),
            payload: json!({}),
        })
    }

    #[test]
    fn session_deletes_jump_ahead_of_pending_serial_reads_without_reordering_each_other() {
        let mut pending_concurrent = VecDeque::new();
        let mut pending_serial = VecDeque::new();
        let mut ready = VecDeque::new();
        for (request_id, path) in [
            ("usage", "/thread-usage-history"),
            ("delete-1", "/session/delete"),
            ("settings", "/settings/set"),
            ("delete-2", "/session/delete"),
        ] {
            queue_bridge_dispatch(
                Some(bridge_call(request_id, path)),
                &mut pending_concurrent,
                &mut pending_serial,
                &mut ready,
            );
        }

        assert!(pending_concurrent.is_empty());
        assert!(ready.is_empty());
        assert_eq!(
            pending_serial
                .iter()
                .map(|call| call.request_id.as_str())
                .collect::<Vec<_>>(),
            ["delete-1", "delete-2", "usage", "settings"]
        );
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
