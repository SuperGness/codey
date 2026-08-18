use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;
use tokio::time::{Instant, timeout};

use super::app_server::{
    AppServerAdapter, FinalDeliveryRequest, ReconcileOutcome, StartNodeRequest, StartedNode,
    SteerThreadRequest,
};
use super::domain::{WorkflowError, WorkflowResult};

const CONTROL_PROTOCOL: u64 = 1;
const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const EXECUTION_POLL_INTERVAL: Duration = Duration::from_millis(700);
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyHealth {
    pub connected: bool,
    pub accepting: bool,
    pub engine_epoch: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadContext {
    pub thread_id: String,
    pub cwd: String,
    pub approval_policy: Value,
    pub sandbox: Value,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub latest_turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionOutcome {
    Succeeded(Value),
    Failed(String),
    Unknown(String),
}

struct ControlConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    epoch: String,
    accepting: bool,
    notifications: VecDeque<Value>,
}

#[derive(Default)]
struct ClientState {
    connection: Option<ControlConnection>,
    last_epoch: Option<String>,
}

#[derive(Clone)]
pub struct WorkflowControlClient {
    address: Arc<str>,
    capability_token: Arc<str>,
    state: Arc<Mutex<ClientState>>,
    next_id: Arc<AtomicU64>,
}

impl WorkflowControlClient {
    pub fn new(address: impl Into<String>, capability_token: impl Into<String>) -> Self {
        Self {
            address: Arc::from(address.into()),
            capability_token: Arc::from(capability_token.into()),
            state: Arc::new(Mutex::new(ClientState::default())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub async fn health(&self) -> ProxyHealth {
        let mut state = self.state.lock().await;
        if state.connection.is_none() {
            match self.connect().await {
                Ok(connection) => {
                    state.last_epoch = Some(connection.epoch.clone());
                    state.connection = Some(connection);
                }
                Err(error) => {
                    return ProxyHealth {
                        connected: false,
                        accepting: false,
                        engine_epoch: state.last_epoch.clone(),
                        reason: Some(error.to_string()),
                    };
                }
            }
        }
        let connection = state.connection.as_ref().expect("connection initialized");
        ProxyHealth {
            connected: true,
            accepting: connection.accepting,
            engine_epoch: Some(connection.epoch.clone()),
            reason: (!connection.accepting)
                .then(|| "app-server proxy is not accepting requests".to_string()),
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> WorkflowResult<Value> {
        self.request_with_ownership(method, params, true).await
    }

    async fn request_unowned(&self, method: &str, params: Value) -> WorkflowResult<Value> {
        self.request_with_ownership(method, params, false).await
    }

    async fn request_with_ownership(
        &self,
        method: &str,
        params: Value,
        own_threads: bool,
    ) -> WorkflowResult<Value> {
        let request_id = format!("engine-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let message = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });
        let mut state = self.state.lock().await;
        if state.connection.is_none() {
            let connection = self.connect().await?;
            state.last_epoch = Some(connection.epoch.clone());
            state.connection = Some(connection);
        }
        let connection = state.connection.as_mut().expect("connection initialized");
        if !connection.accepting {
            return Err(WorkflowError::Unavailable(
                "app-server proxy is not accepting workflow requests".to_string(),
            ));
        }
        let envelope = json!({
            "type": "request",
            "protocol": CONTROL_PROTOCOL,
            "engine_epoch": connection.epoch,
            "own_threads": own_threads,
            "message": message,
        });
        if let Err(error) = write_json_line(&mut connection.writer, &envelope).await {
            state.connection = None;
            return Err(WorkflowError::Adapter(format!(
                "workflow proxy request could not be written: {error}"
            )));
        }

        let response = timeout(REQUEST_TIMEOUT, async {
            loop {
                let envelope = read_json_line(&mut connection.reader).await?;
                match envelope.get("type").and_then(Value::as_str) {
                    Some("response") => {
                        let response = envelope.get("message").cloned().ok_or_else(|| {
                            WorkflowError::Adapter(
                                "workflow proxy returned an empty response".to_string(),
                            )
                        })?;
                        if response.get("id").and_then(Value::as_str) != Some(request_id.as_str()) {
                            continue;
                        }
                        return Ok(response);
                    }
                    Some("notification") => {
                        if let Some(notification) = envelope.get("message") {
                            if connection.notifications.len() >= 256 {
                                connection.notifications.pop_front();
                            }
                            connection.notifications.push_back(notification.clone());
                        }
                    }
                    Some("server_request") => {
                        reply_server_request(connection, &envelope).await?;
                    }
                    Some("health") => {
                        connection.accepting = envelope
                            .get("accepting_workflow_requests")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                    }
                    Some("epoch_changed") | Some("protocol_error") => {
                        return Err(WorkflowError::Adapter(
                            "workflow proxy epoch changed while a request was in flight"
                                .to_string(),
                        ));
                    }
                    _ => {}
                }
            }
        })
        .await;
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                state.connection = None;
                return Err(error);
            }
            Err(_) => {
                state.connection = None;
                return Err(WorkflowError::Adapter(format!(
                    "app-server request {method} timed out; outcome is unknown"
                )));
            }
        };
        if let Some(error) = response.get("error") {
            return Err(WorkflowError::Adapter(format!(
                "app-server {method} failed: {}",
                compact_json(error)
            )));
        }
        response.get("result").cloned().ok_or_else(|| {
            WorkflowError::Adapter(format!("app-server {method} response is missing result"))
        })
    }

    async fn connect(&self) -> WorkflowResult<ControlConnection> {
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(self.address.as_ref()))
            .await
            .map_err(|_| {
                WorkflowError::Unavailable(
                    "timed out connecting to the app-server proxy".to_string(),
                )
            })?
            .map_err(|error| {
                WorkflowError::Unavailable(format!(
                    "app-server proxy control connection is unavailable: {error}"
                ))
            })?;
        let _ = stream.set_nodelay(true);
        let (read_half, mut write_half) = stream.into_split();
        write_json_line(
            &mut write_half,
            &json!({ "type": "auth", "token": self.capability_token.as_ref() }),
        )
        .await?;
        let mut reader = BufReader::new(read_half);
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut epoch = None;
        let mut accepting = false;
        while Instant::now() < deadline && (epoch.is_none() || !accepting) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let envelope = timeout(remaining, read_json_line(&mut reader))
                .await
                .map_err(|_| {
                    WorkflowError::Unavailable(
                        "app-server proxy did not complete authentication".to_string(),
                    )
                })??;
            match envelope.get("type").and_then(Value::as_str) {
                Some("ready") | Some("epoch_changed") => {
                    epoch = envelope
                        .get("engine_epoch")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                }
                Some("health") => {
                    if epoch.is_none() {
                        epoch = envelope
                            .get("engine_epoch")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                    }
                    accepting = envelope
                        .get("accepting_workflow_requests")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                }
                _ => {}
            }
        }
        let epoch = epoch.ok_or_else(|| {
            WorkflowError::Unavailable("app-server proxy omitted its engine epoch".to_string())
        })?;
        Ok(ControlConnection {
            reader,
            writer: write_half,
            epoch,
            accepting,
            notifications: VecDeque::new(),
        })
    }
}

async fn write_json_line(writer: &mut OwnedWriteHalf, value: &Value) -> WorkflowResult<()> {
    let mut frame = serde_json::to_vec(value)?;
    if frame.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(WorkflowError::InvalidRequest(
            "workflow proxy frame exceeds the one MiB limit".to_string(),
        ));
    }
    frame.push(b'\n');
    writer
        .write_all(&frame)
        .await
        .map_err(|error| WorkflowError::Adapter(error.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|error| WorkflowError::Adapter(error.to_string()))
}

async fn read_json_line(reader: &mut BufReader<OwnedReadHalf>) -> WorkflowResult<Value> {
    let mut frame = Vec::new();
    let bytes = reader
        .read_until(b'\n', &mut frame)
        .await
        .map_err(|error| WorkflowError::Adapter(error.to_string()))?;
    if bytes == 0 {
        return Err(WorkflowError::Adapter(
            "workflow proxy control connection closed".to_string(),
        ));
    }
    if frame.len() > MAX_CONTROL_FRAME_BYTES + 1 {
        return Err(WorkflowError::Adapter(
            "workflow proxy returned an oversized frame".to_string(),
        ));
    }
    while matches!(frame.last(), Some(b'\n' | b'\r')) {
        frame.pop();
    }
    serde_json::from_slice(&frame).map_err(WorkflowError::from)
}

async fn reply_server_request(
    connection: &mut ControlConnection,
    envelope: &Value,
) -> WorkflowResult<()> {
    let request = envelope.get("message").cloned().unwrap_or(Value::Null);
    let request_id = request.get("id").cloned().unwrap_or(Value::Null);
    write_json_line(
        &mut connection.writer,
        &json!({
            "type": "server_response",
            "protocol": CONTROL_PROTOCOL,
            "engine_epoch": connection.epoch,
            "message": {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {
                    "code": -32001,
                    "message": "Workflow approval requires explicit Codey review",
                },
            },
        }),
    )
    .await
}

fn compact_json(value: &Value) -> String {
    let rendered = value.to_string();
    rendered.chars().take(500).collect()
}

#[derive(Clone)]
pub struct ProxyAppServerAdapter {
    client: Arc<WorkflowControlClient>,
}

impl ProxyAppServerAdapter {
    pub fn new(client: Arc<WorkflowControlClient>) -> Self {
        Self { client }
    }

    pub async fn inspect_thread(&self, thread_id: &str) -> WorkflowResult<ThreadContext> {
        let mut result = self
            .client
            .request_unowned("thread/resume", thread_metadata_resume_params(thread_id))
            .await?;
        // A freshly rejoined desktop thread can briefly expose the permission
        // profile before its legacy `sandbox` projection is populated. Retry
        // the metadata-only resume once so the immutable snapshot prefers the
        // concrete SandboxPolicy and never hydrates historical turns.
        if resume_sandbox_is_missing(&result) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            result = self
                .client
                .request_unowned("thread/resume", thread_metadata_resume_params(thread_id))
                .await?;
        }
        let sandbox = sandbox_from_resume_result(&result)?;
        Ok(ThreadContext {
            thread_id: result
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
                .unwrap_or(thread_id)
                .to_string(),
            cwd: result
                .get("cwd")
                .and_then(Value::as_str)
                .or_else(|| {
                    result
                        .get("thread")
                        .and_then(|thread| thread.get("cwd"))
                        .and_then(Value::as_str)
                })
                .unwrap_or_default()
                .to_string(),
            approval_policy: result
                .get("approvalPolicy")
                .cloned()
                .unwrap_or_else(|| json!("never")),
            sandbox,
            model: result
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            reasoning_effort: result
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            latest_turn_id: latest_turn_id(result.get("thread")),
        })
    }

    pub async fn start_origin(
        &self,
        cwd: &str,
        approval_policy: Value,
        sandbox: Value,
    ) -> WorkflowResult<ThreadContext> {
        let thread_sandbox = app_server_sandbox_mode(&sandbox)?;
        let result = self
            .client
            .request_unowned(
                "thread/start",
                json!({
                    "cwd": cwd,
                    "approvalPolicy": approval_policy,
                    "sandbox": thread_sandbox,
                    "ephemeral": false,
                    "runtimeWorkspaceRoots": [cwd],
                    "serviceName": "codey_workflow",
                }),
            )
            .await?;
        let thread_id = result
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                WorkflowError::Adapter("thread/start response omitted thread.id".to_string())
            })?
            .to_string();
        Ok(ThreadContext {
            thread_id,
            cwd: result
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or(cwd)
                .to_string(),
            approval_policy: result
                .get("approvalPolicy")
                .cloned()
                .unwrap_or(approval_policy),
            sandbox: result.get("sandbox").cloned().unwrap_or(sandbox),
            model: result
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            reasoning_effort: result
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            latest_turn_id: latest_turn_id(result.get("thread")),
        })
    }

    pub async fn inspect_defaults(&self, cwd: &str) -> WorkflowResult<ThreadContext> {
        let result = self
            .client
            .request("config/read", json!({ "cwd": cwd, "includeLayers": false }))
            .await?;
        let config = result.get("config").unwrap_or(&Value::Null);
        Ok(ThreadContext {
            thread_id: String::new(),
            cwd: cwd.to_string(),
            approval_policy: config
                .get("approval_policy")
                .cloned()
                .unwrap_or_else(|| json!("on-request")),
            sandbox: config
                .get("sandbox_mode")
                .cloned()
                .unwrap_or_else(|| json!("workspace-write")),
            model: config
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            reasoning_effort: config
                .get("model_reasoning_effort")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            latest_turn_id: None,
        })
    }

    pub async fn delete_thread(&self, thread_id: &str) -> WorkflowResult<()> {
        self.client
            .request_unowned("thread/delete", json!({ "threadId": thread_id }))
            .await
            .map(|_| ())
    }

    pub async fn inject_user_once(&self, thread_id: &str, text: &str) -> WorkflowResult<()> {
        self.inject_message_once(thread_id, "user", text).await
    }

    async fn inject_message_once(
        &self,
        thread_id: &str,
        role: &str,
        text: &str,
    ) -> WorkflowResult<()> {
        if self
            .thread_contains_text(thread_id, role, text)
            .await
            .unwrap_or(false)
        {
            return Ok(());
        }
        let content_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        let result = self
            .client
            .request_unowned(
                "thread/inject_items",
                json!({
                    "threadId": thread_id,
                    "items": [{
                        "type": "message",
                        "role": role,
                        "content": [{ "type": content_type, "text": text }],
                    }],
                }),
            )
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) => {
                if self
                    .thread_contains_text(thread_id, role, text)
                    .await
                    .unwrap_or(false)
                {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn thread_contains_text(
        &self,
        thread_id: &str,
        role: &str,
        expected: &str,
    ) -> WorkflowResult<bool> {
        let result = self
            .client
            .request_unowned(
                "thread/read",
                json!({ "threadId": thread_id, "includeTurns": true }),
            )
            .await?;
        Ok(collect_messages(result.get("thread"))
            .into_iter()
            .any(|(candidate_role, text)| candidate_role == role && text == expected))
    }

    pub async fn wait_for_execution(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
    ) -> WorkflowResult<ExecutionOutcome> {
        let deadline = Instant::now() + EXECUTION_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Ok(ExecutionOutcome::Unknown(
                    "worker turn exceeded the execution deadline".to_string(),
                ));
            }
            let result = self
                .client
                .request_unowned(
                    "thread/read",
                    json!({ "threadId": thread_id, "includeTurns": true }),
                )
                .await?;
            let thread = result.get("thread").unwrap_or(&Value::Null);
            if let Some(turn) = select_turn(thread, turn_id) {
                match turn.get("status").and_then(Value::as_str) {
                    Some("completed") => {
                        let text = agent_text_from_turn(turn).unwrap_or_default();
                        return Ok(ExecutionOutcome::Succeeded(json!({ "text": text })));
                    }
                    Some("failed") => {
                        return Ok(ExecutionOutcome::Failed(
                            turn.get("error")
                                .map(compact_json)
                                .unwrap_or_else(|| "worker turn failed".to_string()),
                        ));
                    }
                    Some("interrupted") => {
                        return Ok(ExecutionOutcome::Failed(
                            "worker turn was interrupted".to_string(),
                        ));
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(EXECUTION_POLL_INTERVAL).await;
        }
    }
}

#[async_trait]
impl AppServerAdapter for ProxyAppServerAdapter {
    async fn start_node(&self, request: StartNodeRequest) -> WorkflowResult<StartedNode> {
        if request.cwd.trim().is_empty() {
            return Err(WorkflowError::InvalidRequest(
                "worker thread requires an absolute cwd".to_string(),
            ));
        }
        let sandbox_mode = if request.repo_writer {
            match request.sandbox_mode.as_str() {
                "workspace-write" | "danger-full-access" => "workspaceWrite",
                _ => {
                    return Err(WorkflowError::InvalidRequest(
                        "writer cannot run under an unproven sandbox policy".to_string(),
                    ));
                }
            }
        } else {
            "readOnly"
        };
        let approval_policy = if request.repo_writer {
            request.approval_policy.clone()
        } else {
            Value::String("never".to_string())
        };
        let sandbox_policy = restricted_sandbox_policy(sandbox_mode, &request.cwd);
        let reused_origin = request.existing_thread_id.is_some();
        let thread_id = if let Some(thread_id) = request.existing_thread_id.clone() {
            thread_id
        } else {
            let started = self
                .client
                .request(
                    "thread/start",
                    json!({
                        "cwd": request.cwd,
                        "approvalPolicy": approval_policy,
                        "approvalsReviewer": "auto_review",
                        "sandbox": sandbox_mode,
                        "model": request.model,
                        "ephemeral": false,
                        "runtimeWorkspaceRoots": [request.cwd],
                        "serviceName": "codey_workflow",
                        "developerInstructions": "This is an isolated Codey workflow worker. Follow the supplied role and return evidence to the engine; do not address the user directly.",
                    }),
                )
                .await?;
            started
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WorkflowError::Adapter("thread/start omitted thread.id".to_string())
                })?
                .to_string()
        };
        let prompt = serde_json::to_string_pretty(&request.prompt)?;
        let visible_input = if reused_origin {
            value_to_text(request.prompt.get("objective").unwrap_or(&Value::Null))
        } else {
            prompt.clone()
        };
        let additional_context = reused_origin.then(|| {
            json!({
                "codeyWorkflow": {
                    "kind": "application",
                    "value": prompt,
                }
            })
        });
        let turn_params = json!({
            "threadId": thread_id,
            "input": [{ "type": "text", "text": visible_input }],
            "additionalContext": additional_context,
            "clientUserMessageId": request.idempotency_key,
            "cwd": request.cwd,
            "model": request.model,
            "effort": request.reasoning_effort,
            "approvalPolicy": approval_policy,
            "approvalsReviewer": (!reused_origin).then_some("auto_review"),
            "sandboxPolicy": sandbox_policy,
            "runtimeWorkspaceRoots": [request.cwd],
        });
        let turn = if reused_origin {
            self.client
                .request_unowned("turn/start", turn_params)
                .await?
        } else {
            self.client.request("turn/start", turn_params).await?
        };
        let turn_id = turn
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Ok(StartedNode { thread_id, turn_id })
    }

    async fn steer_thread(&self, request: SteerThreadRequest) -> WorkflowResult<()> {
        let read = self
            .client
            .request_unowned(
                "thread/read",
                json!({ "threadId": request.thread_id, "includeTurns": true }),
            )
            .await?;
        let text = value_to_text(&request.message);
        if let Some(turn_id) = active_turn_id(read.get("thread")) {
            self.client
                .request_unowned(
                    "turn/steer",
                    json!({
                        "threadId": request.thread_id,
                        "expectedTurnId": turn_id,
                        "input": [{ "type": "text", "text": text }],
                        "clientUserMessageId": request.idempotency_key,
                    }),
                )
                .await?;
        } else {
            self.client
                .request_unowned(
                    "turn/start",
                    json!({
                        "threadId": request.thread_id,
                        "input": [{ "type": "text", "text": text }],
                        "clientUserMessageId": request.idempotency_key,
                    }),
                )
                .await?;
        }
        Ok(())
    }

    async fn interrupt_thread(
        &self,
        _run_id: &str,
        thread_id: &str,
        _idempotency_key: &str,
    ) -> WorkflowResult<()> {
        let read = self
            .client
            .request_unowned(
                "thread/read",
                json!({ "threadId": thread_id, "includeTurns": true }),
            )
            .await?;
        if let Some(turn_id) = active_turn_id(read.get("thread")) {
            self.client
                .request_unowned(
                    "turn/interrupt",
                    json!({ "threadId": thread_id, "turnId": turn_id }),
                )
                .await?;
        }
        Ok(())
    }

    async fn reconcile_thread(
        &self,
        _run_id: &str,
        _node_id: &str,
        thread_id: &str,
    ) -> WorkflowResult<ReconcileOutcome> {
        let read = match self
            .client
            .request_unowned(
                "thread/read",
                json!({ "threadId": thread_id, "includeTurns": true }),
            )
            .await
        {
            Ok(value) => value,
            Err(WorkflowError::Adapter(message)) if message.contains("not found") => {
                return Ok(ReconcileOutcome::NotFound);
            }
            Err(error) => return Err(error),
        };
        let thread = read.get("thread").unwrap_or(&Value::Null);
        if thread_waiting_for_approval(thread) {
            return Ok(ReconcileOutcome::WaitingApproval);
        }
        let Some(turn) = select_turn(thread, None) else {
            return Ok(ReconcileOutcome::NotFound);
        };
        Ok(match turn.get("status").and_then(Value::as_str) {
            Some("completed") => ReconcileOutcome::Succeeded(json!({
                "text": agent_text_from_turn(turn).unwrap_or_default()
            })),
            Some("failed" | "interrupted") => ReconcileOutcome::Failed,
            Some("inProgress") => ReconcileOutcome::Running,
            _ => ReconcileOutcome::Unknown,
        })
    }

    async fn deliver_final(&self, request: FinalDeliveryRequest) -> WorkflowResult<()> {
        let text = value_to_text(&request.content);
        self.inject_message_once(&request.origin_thread_id, "assistant", &text)
            .await
    }
}

fn thread_metadata_resume_params(thread_id: &str) -> Value {
    json!({
        "threadId": thread_id,
        // Capability preflight only needs the frozen execution context. Asking
        // app-server to hydrate every historical turn can make a long origin
        // thread exceed the bounded proxy control frame before the workflow is
        // even admitted.
        "excludeTurns": true,
    })
}

fn resume_sandbox_is_missing(result: &Value) -> bool {
    result.get("sandbox").is_none_or(Value::is_null)
}

fn sandbox_from_resume_result(result: &Value) -> WorkflowResult<Value> {
    if let Some(sandbox) = result.get("sandbox").filter(|value| !value.is_null()) {
        return Ok(sandbox.clone());
    }

    let profile_id = result
        .get("activePermissionProfile")
        .and_then(|profile| profile.get("id"))
        .and_then(Value::as_str);
    let mode = match profile_id {
        Some(":read-only") => "read-only",
        Some(":workspace") => "workspace-write",
        Some(":danger-full-access") => "danger-full-access",
        Some(_) => {
            return Err(WorkflowError::InvalidRequest(
                "sandbox policy is unavailable and the active permission profile is custom"
                    .to_string(),
            ));
        }
        None => {
            return Err(WorkflowError::InvalidRequest(
                "sandbox policy is unavailable after refreshing thread metadata".to_string(),
            ));
        }
    };
    Ok(Value::String(mode.to_string()))
}

fn app_server_sandbox_mode(value: &Value) -> WorkflowResult<&'static str> {
    let mode = value
        .as_str()
        .or_else(|| value.get("type").and_then(Value::as_str));
    match mode {
        Some("read-only" | "readOnly") => Ok("readOnly"),
        Some("workspace-write" | "workspaceWrite") => Ok("workspaceWrite"),
        Some("danger-full-access" | "dangerFullAccess") => Ok("dangerFullAccess"),
        _ => Err(WorkflowError::InvalidRequest(
            "sandbox policy cannot be converted to an app-server mode".to_string(),
        )),
    }
}

fn restricted_sandbox_policy(mode: &str, cwd: &str) -> Value {
    if mode == "workspaceWrite" {
        json!({
            "type": "workspaceWrite",
            "writableRoots": [cwd],
            "networkAccess": false,
        })
    } else {
        json!({
            "type": "readOnly",
            "networkAccess": false,
        })
    }
}

fn value_to_text(value: &Value) -> String {
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        })
}

fn latest_turn_id(thread: Option<&Value>) -> Option<String> {
    thread
        .and_then(|thread| thread.get("turns"))
        .and_then(Value::as_array)
        .and_then(|turns| turns.last())
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn active_turn_id(thread: Option<&Value>) -> Option<String> {
    thread
        .and_then(|thread| thread.get("turns"))
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns
                .iter()
                .rev()
                .find(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))
        })
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn select_turn<'a>(thread: &'a Value, turn_id: Option<&str>) -> Option<&'a Value> {
    let turns = thread.get("turns")?.as_array()?;
    turn_id
        .and_then(|expected| {
            turns
                .iter()
                .find(|turn| turn.get("id").and_then(Value::as_str) == Some(expected))
        })
        .or_else(|| turns.last())
}

fn thread_waiting_for_approval(thread: &Value) -> bool {
    let status = thread.get("status").unwrap_or(&Value::Null);
    status.get("type").and_then(Value::as_str) == Some("active")
        && status
            .get("activeFlags")
            .and_then(Value::as_array)
            .is_some_and(|flags| {
                flags
                    .iter()
                    .any(|flag| flag.as_str() == Some("waitingOnApproval"))
            })
}

fn agent_text_from_turn(turn: &Value) -> Option<String> {
    turn.get("items")?
        .as_array()?
        .iter()
        .rev()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("agentMessage"))
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn collect_messages(thread: Option<&Value>) -> Vec<(String, String)> {
    let mut messages = Vec::new();
    let Some(turns) = thread
        .and_then(|thread| thread.get("turns"))
        .and_then(Value::as_array)
    else {
        return messages;
    };
    for turn in turns {
        let Some(items) = turn.get("items").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("agentMessage") => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        messages.push(("assistant".to_string(), text.to_string()));
                    }
                }
                Some("userMessage") => {
                    if let Some(text) = item
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|content| content.iter().find_map(|part| part.get("text")))
                        .and_then(Value::as_str)
                    {
                        messages.push(("user".to_string(), text.to_string()));
                    }
                }
                _ => {}
            }
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_server_sandbox_modes_use_official_camel_case_values() {
        assert_eq!(
            app_server_sandbox_mode(&json!("workspace-write")).unwrap(),
            "workspaceWrite"
        );
        assert_eq!(
            app_server_sandbox_mode(&json!({"type": "readOnly"})).unwrap(),
            "readOnly"
        );
        assert!(app_server_sandbox_mode(&json!("unknown")).is_err());
    }

    #[test]
    fn worker_sandbox_is_restricted_to_the_frozen_workspace() {
        let policy = restricted_sandbox_policy("workspaceWrite", "/repo");
        assert_eq!(policy.get("type"), Some(&json!("workspaceWrite")));
        assert_eq!(policy.get("writableRoots"), Some(&json!(["/repo"])));
        assert_eq!(policy.get("networkAccess"), Some(&json!(false)));
        assert!(policy.get("readOnlyAccess").is_none());
        assert!(policy.get("access").is_none());

        let read_only = restricted_sandbox_policy("readOnly", "/repo");
        assert_eq!(
            read_only,
            json!({"type": "readOnly", "networkAccess": false})
        );
    }

    #[test]
    fn composer_preflight_resumes_only_thread_metadata() {
        assert_eq!(
            thread_metadata_resume_params("origin-thread"),
            json!({
                "threadId": "origin-thread",
                "excludeTurns": true,
            })
        );
    }

    #[test]
    fn composer_preflight_recovers_only_proven_builtin_permission_profiles() {
        for (profile, expected) in [
            (":read-only", "read-only"),
            (":workspace", "workspace-write"),
            (":danger-full-access", "danger-full-access"),
        ] {
            let result = json!({
                "sandbox": null,
                "activePermissionProfile": { "id": profile },
            });
            assert_eq!(
                sandbox_from_resume_result(&result).unwrap(),
                json!(expected)
            );
        }

        assert!(
            sandbox_from_resume_result(&json!({
                "sandbox": null,
                "activePermissionProfile": { "id": "company-managed" },
            }))
            .is_err()
        );
        assert!(sandbox_from_resume_result(&json!({ "sandbox": null })).is_err());
    }

    #[test]
    fn composer_preflight_preserves_a_concrete_sandbox_policy() {
        let policy = json!({
            "type": "workspaceWrite",
            "writableRoots": ["/repo"],
            "networkAccess": false,
        });
        assert_eq!(
            sandbox_from_resume_result(&json!({
                "sandbox": policy.clone(),
                "activePermissionProfile": { "id": ":read-only" },
            }))
            .unwrap(),
            policy
        );
    }
}
