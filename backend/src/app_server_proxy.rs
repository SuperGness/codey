use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

pub(crate) const HELPER_ARGUMENT: &str = "--codey-app-server-proxy";
pub(crate) const ENV_ENABLED: &str = "CODEY_WORKFLOW_PROXY_ENABLED";
pub(crate) const ENV_EXECUTABLE: &str = "CODEY_WORKFLOW_PROXY_EXECUTABLE";
pub(crate) const ENV_CONTROL_ADDRESS: &str = "CODEY_WORKFLOW_PROXY_CONTROL_ADDR";
pub(crate) const ENV_CAPABILITY_TOKEN: &str = "CODEY_WORKFLOW_PROXY_TOKEN";
pub(crate) const ENV_BYPASS: &str = "CODEY_WORKFLOW_PROXY_BYPASS";

const CODEX_EXECUTABLE_ARGUMENT: &str = "--codex-executable";
const PROTOCOL_VERSION: u64 = 1;
const CHILD_INPUT_QUEUE_CAPACITY: usize = 256;
const CONTROL_OUTPUT_QUEUE_CAPACITY: usize = 128;
const MAX_APP_SERVER_LINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONTROL_LINE_BYTES: usize = 1024 * 1024;
const CONTROL_AUTH_TIMEOUT: Duration = Duration::from_secs(3);
const TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const INTERNAL_REQUEST_PREFIX: &str = "codey-workflow:";

struct HelperConfig {
    codex_executable: OsString,
    codex_arguments: Vec<OsString>,
    control: Option<ControlConfig>,
}

struct ControlConfig {
    address: SocketAddr,
    capability_token: Vec<u8>,
}

#[derive(Clone)]
struct ControlSession {
    generation: u64,
    epoch: String,
    sender: mpsc::Sender<Vec<u8>>,
}

struct PendingWorkflowRequest {
    epoch: String,
    original_id: Value,
    own_threads: bool,
}

struct RoutingState {
    epoch: String,
    previous_epoch: Option<String>,
    control: Option<ControlSession>,
    pending_workflow_requests: HashMap<String, PendingWorkflowRequest>,
    workflow_owned_threads: HashSet<String>,
    workflow_server_requests: HashMap<String, String>,
}

struct SharedState {
    routing: Mutex<RoutingState>,
    child_input: mpsc::Sender<ChildInput>,
    next_request_sequence: AtomicU64,
    next_control_generation: AtomicU64,
    child_alive: AtomicBool,
    dropped_control_messages: AtomicU64,
}

struct ControlInstall {
    generation: u64,
    epoch: String,
    previous_epoch: Option<String>,
}

enum ChildInput {
    Frame(Vec<u8>),
    Close,
}

enum InternalResponse {
    NotInternal,
    Orphaned,
    Deliver { epoch: String, message: Value },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageKind {
    Notification,
    Request,
    Response,
    Other,
}

pub(crate) fn run_helper_if_requested() -> Result<Option<i32>> {
    let Some(config) = HelperConfig::from_process_arguments()? else {
        return Ok(None);
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("无法启动 Codey app-server proxy runtime")?;
    runtime.block_on(run_proxy(config)).map(Some)
}

impl HelperConfig {
    fn from_process_arguments() -> Result<Option<Self>> {
        Self::from_arguments(env::args_os().skip(1))
    }

    fn from_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<Option<Self>> {
        let mut arguments = arguments.into_iter();
        let Some(mode) = arguments.next() else {
            return Ok(None);
        };
        if mode != OsStr::new(HELPER_ARGUMENT) {
            return Ok(None);
        }

        if arguments.next().as_deref() != Some(OsStr::new(CODEX_EXECUTABLE_ARGUMENT)) {
            bail!("app-server proxy helper 缺少真实 Codex 可执行文件参数");
        }
        let codex_executable = arguments
            .next()
            .context("app-server proxy helper 缺少真实 Codex 可执行文件")?;
        if arguments.next().as_deref() != Some(OsStr::new("--")) {
            bail!("app-server proxy helper 参数分隔符无效");
        }
        let codex_arguments = arguments.collect::<Vec<_>>();
        validate_real_codex_invocation(&codex_executable, &codex_arguments)?;

        Ok(Some(Self {
            codex_executable,
            codex_arguments,
            control: ControlConfig::from_environment(),
        }))
    }
}

impl ControlConfig {
    fn from_environment() -> Option<Self> {
        if env::var(ENV_ENABLED).ok().as_deref() != Some("1") {
            return None;
        }
        let address = env::var(ENV_CONTROL_ADDRESS).ok()?.parse().ok()?;
        if !is_private_control_address(address) {
            return None;
        }
        let capability_token = env::var(ENV_CAPABILITY_TOKEN).ok()?.into_bytes();
        if !is_valid_capability_token(&capability_token) {
            return None;
        }
        Some(Self {
            address,
            capability_token,
        })
    }
}

impl SharedState {
    fn new(child_input: mpsc::Sender<ChildInput>) -> Self {
        Self {
            routing: Mutex::new(RoutingState {
                epoch: new_engine_epoch(),
                previous_epoch: None,
                control: None,
                pending_workflow_requests: HashMap::new(),
                workflow_owned_threads: HashSet::new(),
                workflow_server_requests: HashMap::new(),
            }),
            child_input,
            next_request_sequence: AtomicU64::new(1),
            next_control_generation: AtomicU64::new(1),
            child_alive: AtomicBool::new(true),
            dropped_control_messages: AtomicU64::new(0),
        }
    }

    fn routing(&self) -> MutexGuard<'_, RoutingState> {
        self.routing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn install_control(&self, sender: mpsc::Sender<Vec<u8>>) -> ControlInstall {
        let generation = self.next_control_generation.fetch_add(1, Ordering::Relaxed);
        let mut routing = self.routing();
        let install = ControlInstall {
            generation,
            epoch: routing.epoch.clone(),
            previous_epoch: routing.previous_epoch.take(),
        };
        routing.control = Some(ControlSession {
            generation,
            epoch: install.epoch.clone(),
            sender,
        });
        install
    }

    fn disconnect_control(&self, generation: u64) {
        let mut routing = self.routing();
        if routing.control.as_ref().map(|session| session.generation) != Some(generation) {
            return;
        }
        routing.control = None;
        let previous_epoch = std::mem::replace(&mut routing.epoch, new_engine_epoch());
        routing.previous_epoch = Some(previous_epoch);
        routing.pending_workflow_requests.clear();
        routing.workflow_owned_threads.clear();
        routing.workflow_server_requests.clear();
    }

    fn current_epoch(&self) -> String {
        self.routing().epoch.clone()
    }

    fn current_epoch_for_generation(&self, generation: u64) -> Option<String> {
        let routing = self.routing();
        let control = routing.control.as_ref()?;
        (control.generation == generation && control.epoch == routing.epoch)
            .then(|| routing.epoch.clone())
    }

    fn register_workflow_request(
        &self,
        generation: u64,
        expected_epoch: &str,
        message: &Value,
        original_id: Value,
        own_threads: bool,
    ) -> Option<String> {
        if !self.child_alive.load(Ordering::Acquire) {
            return None;
        }
        let sequence = self.next_request_sequence.fetch_add(1, Ordering::Relaxed);
        let mut routing = self.routing();
        let control = routing.control.as_ref()?;
        if control.generation != generation
            || control.epoch != expected_epoch
            || routing.epoch != expected_epoch
        {
            return None;
        }
        let internal_id = format_internal_request_id(expected_epoch, sequence);
        routing.pending_workflow_requests.insert(
            internal_id.clone(),
            PendingWorkflowRequest {
                epoch: expected_epoch.to_owned(),
                original_id,
                own_threads,
            },
        );
        if own_threads {
            routing
                .workflow_owned_threads
                .extend(extract_thread_ids(message));
        }
        Some(internal_id)
    }

    fn unregister_workflow_request(&self, internal_id: &str) {
        self.routing().pending_workflow_requests.remove(internal_id);
    }

    fn take_internal_response(&self, message: &mut Value) -> InternalResponse {
        let Some(internal_id) = message.get("id").and_then(Value::as_str) else {
            return InternalResponse::NotInternal;
        };
        if !internal_id.starts_with(INTERNAL_REQUEST_PREFIX) {
            return InternalResponse::NotInternal;
        }

        let mut routing = self.routing();
        let Some(pending) = routing.pending_workflow_requests.remove(internal_id) else {
            return InternalResponse::Orphaned;
        };
        if let Some(object) = message.as_object_mut() {
            object.insert("id".to_owned(), pending.original_id);
        }
        if pending.own_threads {
            routing
                .workflow_owned_threads
                .extend(extract_thread_ids(message));
        }
        InternalResponse::Deliver {
            epoch: pending.epoch,
            message: message.clone(),
        }
    }

    fn try_send_to_control(&self, expected_epoch: &str, envelope: Value) -> bool {
        let frame = json_line(envelope);
        let sender = {
            let routing = self.routing();
            let Some(control) = routing.control.as_ref() else {
                return false;
            };
            if control.epoch != expected_epoch || routing.epoch != expected_epoch {
                return false;
            }
            control.sender.clone()
        };
        match sender.try_send(frame) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_control_messages
                    .fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn try_send_to_generation(&self, generation: u64, envelope: Value) -> bool {
        let frame = json_line(envelope);
        let sender = {
            let routing = self.routing();
            let Some(control) = routing.control.as_ref() else {
                return false;
            };
            if control.generation != generation {
                return false;
            }
            control.sender.clone()
        };
        match sender.try_send(frame) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_control_messages
                    .fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn try_copy_notification(&self, message: &Value) {
        let epoch = self.current_epoch();
        self.observe_thread_lifecycle(message);
        let _ = self.try_send_to_control(
            &epoch,
            json!({
                "type": "notification",
                "protocol": PROTOCOL_VERSION,
                "engine_epoch": epoch,
                "message": message,
            }),
        );
    }

    fn try_route_approval(&self, message: &Value) -> bool {
        let message_threads = extract_thread_ids(message);
        if message_threads.is_empty() {
            return false;
        }
        let Some(request_id) = message.get("id").and_then(json_id_key) else {
            return false;
        };
        let mut routing = self.routing();
        if !message_threads
            .iter()
            .any(|thread_id| routing.workflow_owned_threads.contains(thread_id))
        {
            return false;
        }
        let Some(control) = routing.control.as_ref().cloned() else {
            return false;
        };
        if control.epoch != routing.epoch {
            return false;
        }
        let frame = json_line(json!({
            "type": "server_request",
            "protocol": PROTOCOL_VERSION,
            "engine_epoch": control.epoch,
            "message": message,
        }));
        match control.sender.try_send(frame) {
            Ok(()) => {
                routing
                    .workflow_server_requests
                    .insert(request_id, control.epoch);
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_control_messages
                    .fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn take_workflow_server_request(&self, epoch: &str, request_id: &Value) -> bool {
        let Some(request_id) = json_id_key(request_id) else {
            return false;
        };
        let mut routing = self.routing();
        if routing
            .workflow_server_requests
            .get(&request_id)
            .map(String::as_str)
            != Some(epoch)
        {
            return false;
        }
        routing.workflow_server_requests.remove(&request_id);
        true
    }

    fn is_workflow_server_request(&self, request_id: &Value) -> bool {
        let Some(request_id) = json_id_key(request_id) else {
            return false;
        };
        self.routing()
            .workflow_server_requests
            .contains_key(&request_id)
    }

    fn observe_thread_lifecycle(&self, message: &Value) {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return;
        };
        if !matches!(
            method,
            "thread/closed" | "thread/deleted" | "thread/archived"
        ) {
            return;
        }
        let thread_ids = extract_thread_ids(message);
        let mut routing = self.routing();
        for thread_id in thread_ids {
            routing.workflow_owned_threads.remove(&thread_id);
        }
    }

    fn release_thread(&self, generation: u64, epoch: &str, thread_id: &str) -> bool {
        let mut routing = self.routing();
        let Some(control) = routing.control.as_ref() else {
            return false;
        };
        if control.generation != generation || routing.epoch != epoch {
            return false;
        }
        routing.workflow_owned_threads.remove(thread_id)
    }

    fn mark_child_stopped(&self, status: &ExitStatus) {
        self.child_alive.store(false, Ordering::Release);
        let epoch = self.current_epoch();
        let _ = self.try_send_to_control(
            &epoch,
            health_envelope(
                &epoch,
                "stopped",
                status.code(),
                self.dropped_control_messages.load(Ordering::Relaxed),
            ),
        );
    }

    fn protocol_error(&self, generation: u64, code: &str) {
        let Some(epoch) = self.current_epoch_for_generation(generation) else {
            return;
        };
        let _ = self.try_send_to_generation(
            generation,
            json!({
                "type": "protocol_error",
                "protocol": PROTOCOL_VERSION,
                "engine_epoch": epoch,
                "code": code,
            }),
        );
    }
}

async fn run_proxy(config: HelperConfig) -> Result<i32> {
    let mut command = Command::new(&config.codex_executable);
    command
        .args(&config.codex_arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_remove(ENV_ENABLED)
        .env_remove(ENV_EXECUTABLE)
        .env_remove(ENV_CONTROL_ADDRESS)
        .env_remove(ENV_CAPABILITY_TOKEN)
        .env(ENV_BYPASS, "1");
    let mut child = command.spawn().context("无法启动真实 codex app-server")?;
    let child_stdin = child.stdin.take().context("真实 app-server stdin 不可用")?;
    let child_stdout = child
        .stdout
        .take()
        .context("真实 app-server stdout 不可用")?;
    let child_stderr = child
        .stderr
        .take()
        .context("真实 app-server stderr 不可用")?;

    let (child_input_sender, child_input_receiver) = mpsc::channel(CHILD_INPUT_QUEUE_CAPACITY);
    let shared = Arc::new(SharedState::new(child_input_sender));

    let child_input_task = tokio::spawn(write_child_input(child_stdin, child_input_receiver));
    let electron_input_task = tokio::spawn(read_electron_input(Arc::clone(&shared)));
    let child_output_task = tokio::spawn(read_child_output(child_stdout, Arc::clone(&shared)));
    let child_stderr_task = tokio::spawn(copy_child_stderr(child_stderr));
    let control_task = if let Some(control) = config.control {
        match TcpListener::bind(control.address).await {
            Ok(listener) => Some(tokio::spawn(run_control_listener(
                listener,
                control.capability_token,
                Arc::clone(&shared),
            ))),
            Err(_) => None,
        }
    } else {
        None
    };

    let status = tokio::select! {
        status = child.wait() => status.context("等待真实 app-server 退出失败")?,
        () = shutdown_signal() => {
            let _ = child.start_kill();
            child.wait().await.context("停止真实 app-server 失败")?
        }
    };

    shared.mark_child_stopped(&status);
    let _ = shared.child_input.try_send(ChildInput::Close);
    electron_input_task.abort();
    if let Some(control_task) = control_task {
        control_task.abort();
    }
    drain_task(child_output_task).await;
    drain_task(child_stderr_task).await;
    drain_task(child_input_task).await;

    Ok(status.code().unwrap_or(1))
}

async fn drain_task(task: JoinHandle<io::Result<()>>) {
    let mut task = task;
    if timeout(TASK_DRAIN_TIMEOUT, &mut task).await.is_err() {
        task.abort();
    }
}

async fn write_child_input(
    mut child_stdin: ChildStdin,
    mut receiver: mpsc::Receiver<ChildInput>,
) -> io::Result<()> {
    while let Some(input) = receiver.recv().await {
        match input {
            ChildInput::Frame(frame) => {
                child_stdin.write_all(&frame).await?;
                child_stdin.flush().await?;
            }
            ChildInput::Close => break,
        }
    }
    child_stdin.shutdown().await
}

async fn read_electron_input(shared: Arc<SharedState>) -> io::Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    while let Some(frame) = read_limited_frame(&mut input, MAX_APP_SERVER_LINE_BYTES).await? {
        if let Ok(message) = serde_json::from_slice::<Value>(&frame)
            && message_kind(&message) == MessageKind::Response
            && message
                .get("id")
                .is_some_and(|request_id| shared.is_workflow_server_request(request_id))
        {
            continue;
        }
        if shared
            .child_input
            .send(ChildInput::Frame(frame))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
    let _ = shared.child_input.send(ChildInput::Close).await;
    Ok(())
}

async fn read_child_output(child_stdout: ChildStdout, shared: Arc<SharedState>) -> io::Result<()> {
    let mut input = BufReader::new(child_stdout);
    let mut output = tokio::io::stdout();
    while let Some(frame) = read_limited_frame(&mut input, MAX_APP_SERVER_LINE_BYTES).await? {
        let Ok(mut message) = serde_json::from_slice::<Value>(&frame) else {
            output.write_all(&frame).await?;
            output.flush().await?;
            continue;
        };

        match message_kind(&message) {
            MessageKind::Notification => {
                output.write_all(&frame).await?;
                output.flush().await?;
                shared.try_copy_notification(&message);
            }
            MessageKind::Response => match shared.take_internal_response(&mut message) {
                InternalResponse::NotInternal => {
                    output.write_all(&frame).await?;
                    output.flush().await?;
                }
                InternalResponse::Orphaned => {}
                InternalResponse::Deliver { epoch, message } => {
                    let _ = shared.try_send_to_control(
                        &epoch,
                        json!({
                            "type": "response",
                            "protocol": PROTOCOL_VERSION,
                            "engine_epoch": epoch,
                            "message": message,
                        }),
                    );
                }
            },
            MessageKind::Request => {
                let approval = message
                    .get("method")
                    .and_then(Value::as_str)
                    .is_some_and(is_approval_method);
                if !approval || !shared.try_route_approval(&message) {
                    output.write_all(&frame).await?;
                    output.flush().await?;
                }
            }
            MessageKind::Other => {
                output.write_all(&frame).await?;
                output.flush().await?;
            }
        }
    }
    output.flush().await
}

async fn copy_child_stderr(mut child_stderr: ChildStderr) -> io::Result<()> {
    let mut stderr = tokio::io::stderr();
    tokio::io::copy(&mut child_stderr, &mut stderr).await?;
    stderr.flush().await
}

async fn run_control_listener(
    listener: TcpListener,
    capability_token: Vec<u8>,
    shared: Arc<SharedState>,
) -> io::Result<()> {
    loop {
        let (stream, peer_address) = listener.accept().await?;
        if !peer_address.ip().is_loopback() {
            continue;
        }
        let _ = handle_control_connection(stream, &capability_token, Arc::clone(&shared)).await;
    }
}

async fn handle_control_connection(
    stream: TcpStream,
    capability_token: &[u8],
    shared: Arc<SharedState>,
) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut input = BufReader::new(read_half);
    let authentication = timeout(
        CONTROL_AUTH_TIMEOUT,
        read_limited_frame(&mut input, MAX_CONTROL_LINE_BYTES),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "control authentication timed out"))??
    .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "control connection closed"))?;
    let authentication: Value = serde_json::from_slice(&authentication).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "invalid control authentication")
    })?;
    let supplied_token = authentication
        .get("token")
        .and_then(Value::as_str)
        .map(str::as_bytes)
        .unwrap_or_default();
    if authentication.get("type").and_then(Value::as_str) != Some("auth")
        || !constant_time_eq(supplied_token, capability_token)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control authentication failed",
        ));
    }

    let (control_sender, mut control_receiver) =
        mpsc::channel::<Vec<u8>>(CONTROL_OUTPUT_QUEUE_CAPACITY);
    let install = shared.install_control(control_sender.clone());
    let mut writer_task = tokio::spawn(async move {
        while let Some(frame) = control_receiver.recv().await {
            write_half.write_all(&frame).await?;
            write_half.flush().await?;
        }
        Ok::<(), io::Error>(())
    });

    control_sender
        .send(json_line(json!({
            "type": "ready",
            "protocol": PROTOCOL_VERSION,
            "engine_epoch": install.epoch,
        })))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control writer stopped"))?;
    if let Some(previous_epoch) = install.previous_epoch {
        control_sender
            .send(json_line(json!({
                "type": "epoch_changed",
                "protocol": PROTOCOL_VERSION,
                "previous_epoch": previous_epoch,
                "engine_epoch": install.epoch,
            })))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control writer stopped"))?;
    }
    control_sender
        .send(json_line(health_envelope(
            &install.epoch,
            if shared.child_alive.load(Ordering::Acquire) {
                "ready"
            } else {
                "stopped"
            },
            None,
            shared.dropped_control_messages.load(Ordering::Relaxed),
        )))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control writer stopped"))?;

    let result = loop {
        tokio::select! {
            frame = read_limited_frame(&mut input, MAX_CONTROL_LINE_BYTES) => {
                match frame {
                    Ok(Some(frame)) => {
                        handle_control_frame(
                            &frame,
                            install.generation,
                            Arc::clone(&shared),
                        ).await;
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(error),
                }
            }
            writer = &mut writer_task => {
                break writer
                    .map_err(|error| io::Error::other(error.to_string()))
                    .and_then(|result| result);
            }
        }
    };
    writer_task.abort();
    shared.disconnect_control(install.generation);
    result
}

async fn handle_control_frame(frame: &[u8], generation: u64, shared: Arc<SharedState>) {
    let Ok(envelope) = serde_json::from_slice::<Value>(frame) else {
        shared.protocol_error(generation, "invalid_json");
        return;
    };
    let Some(message_type) = envelope.get("type").and_then(Value::as_str) else {
        shared.protocol_error(generation, "missing_type");
        return;
    };
    let Some(expected_epoch) = envelope.get("engine_epoch").and_then(Value::as_str) else {
        shared.protocol_error(generation, "missing_epoch");
        return;
    };
    if shared.current_epoch_for_generation(generation).as_deref() != Some(expected_epoch) {
        shared.protocol_error(generation, "stale_epoch");
        return;
    }

    match message_type {
        "request" => {
            let Some(mut message) = envelope.get("message").cloned() else {
                shared.protocol_error(generation, "missing_message");
                return;
            };
            if message_kind(&message) != MessageKind::Request {
                shared.protocol_error(generation, "invalid_request");
                return;
            }
            let Some(original_id) = message.get("id").filter(|id| is_valid_json_id(id)).cloned()
            else {
                shared.protocol_error(generation, "invalid_request_id");
                return;
            };
            let own_threads = envelope
                .get("own_threads")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let Some(internal_id) = shared.register_workflow_request(
                generation,
                expected_epoch,
                &message,
                original_id,
                own_threads,
            ) else {
                shared.protocol_error(generation, "not_accepting_requests");
                return;
            };
            if let Some(object) = message.as_object_mut() {
                object.insert("id".to_owned(), Value::String(internal_id.clone()));
            }
            if shared
                .child_input
                .send(ChildInput::Frame(json_line(message)))
                .await
                .is_err()
            {
                shared.unregister_workflow_request(&internal_id);
                shared.protocol_error(generation, "app_server_stopped");
            }
        }
        "notification" => {
            let Some(message) = envelope.get("message") else {
                shared.protocol_error(generation, "missing_message");
                return;
            };
            if message_kind(message) != MessageKind::Notification {
                shared.protocol_error(generation, "invalid_notification");
                return;
            }
            if shared
                .child_input
                .send(ChildInput::Frame(json_line(message.clone())))
                .await
                .is_err()
            {
                shared.protocol_error(generation, "app_server_stopped");
            }
        }
        "server_response" => {
            let Some(message) = envelope.get("message") else {
                shared.protocol_error(generation, "missing_message");
                return;
            };
            if message_kind(message) != MessageKind::Response {
                shared.protocol_error(generation, "invalid_server_response");
                return;
            }
            let Some(request_id) = message.get("id") else {
                shared.protocol_error(generation, "invalid_server_response_id");
                return;
            };
            if !shared.take_workflow_server_request(expected_epoch, request_id) {
                shared.protocol_error(generation, "unknown_server_request");
                return;
            }
            if shared
                .child_input
                .send(ChildInput::Frame(json_line(message.clone())))
                .await
                .is_err()
            {
                shared.protocol_error(generation, "app_server_stopped");
            }
        }
        "release_thread" => {
            let Some(thread_id) = envelope.get("thread_id").and_then(Value::as_str) else {
                shared.protocol_error(generation, "missing_thread_id");
                return;
            };
            if !shared.release_thread(generation, expected_epoch, thread_id) {
                shared.protocol_error(generation, "thread_not_owned");
            }
        }
        _ => shared.protocol_error(generation, "unknown_envelope_type"),
    }
}

async fn read_limited_frame<R>(reader: &mut R, maximum_bytes: usize) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Ok(Some(frame))
            };
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if frame.len().saturating_add(consumed) > maximum_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSONL frame exceeds the configured limit",
            ));
        }
        frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if frame.last() == Some(&b'\n') {
            return Ok(Some(frame));
        }
    }
}

fn validate_real_codex_invocation(executable: &OsStr, arguments: &[OsString]) -> Result<()> {
    let executable_name = Path::new(executable)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    if !matches_codex_executable_name(executable_name) {
        bail!("app-server proxy helper 只允许启动真实 Codex 可执行文件");
    }
    let app_server_count = arguments
        .iter()
        .filter(|argument| argument.as_os_str() == OsStr::new("app-server"))
        .count();
    if app_server_count != 1
        || arguments
            .iter()
            .any(|argument| argument.as_os_str() == OsStr::new(HELPER_ARGUMENT))
    {
        bail!("app-server proxy helper 只允许 app-server 子命令");
    }
    Ok(())
}

fn matches_codex_executable_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("codex") || name.eq_ignore_ascii_case("codex.exe")
}

fn is_private_control_address(address: SocketAddr) -> bool {
    address.ip().is_loopback() && address.port() != 0
}

fn is_valid_capability_token(token: &[u8]) -> bool {
    (32..=512).contains(&token.len())
        && token
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let maximum_length = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum_length {
        let left_byte = left.get(index).copied().unwrap_or_default();
        let right_byte = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

fn new_engine_epoch() -> String {
    Uuid::new_v4().simple().to_string()
}

fn format_internal_request_id(epoch: &str, sequence: u64) -> String {
    format!("{INTERNAL_REQUEST_PREFIX}{epoch}:{sequence}")
}

fn json_line(value: Value) -> Vec<u8> {
    let mut frame = serde_json::to_vec(&value).expect("JSON value should always serialize");
    frame.push(b'\n');
    frame
}

fn message_kind(message: &Value) -> MessageKind {
    let Some(object) = message.as_object() else {
        return MessageKind::Other;
    };
    let has_method = object.get("method").and_then(Value::as_str).is_some();
    let has_id = object.get("id").is_some_and(|id| !id.is_null());
    match (has_method, has_id) {
        (true, true) => MessageKind::Request,
        (true, false) => MessageKind::Notification,
        (false, true) => MessageKind::Response,
        (false, false) => MessageKind::Other,
    }
}

fn is_valid_json_id(id: &Value) -> bool {
    id.is_string() || id.is_number()
}

fn json_id_key(id: &Value) -> Option<String> {
    is_valid_json_id(id)
        .then(|| serde_json::to_string(id).expect("JSON-RPC identifiers should always serialize"))
}

fn is_approval_method(method: &str) -> bool {
    method.to_ascii_lowercase().contains("approval")
}

fn extract_thread_ids(message: &Value) -> HashSet<String> {
    fn scalar_id(value: &Value) -> Option<String> {
        match value {
            Value::String(value) if !value.is_empty() => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        }
    }

    fn visit(value: &Value, output: &mut HashSet<String>, depth: usize) {
        if depth > 12 {
            return;
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, output, depth + 1);
                }
            }
            Value::Object(object) => {
                for (key, value) in object {
                    let normalized = key
                        .chars()
                        .filter(|character| character.is_ascii_alphanumeric())
                        .flat_map(char::to_lowercase)
                        .collect::<String>();
                    if matches!(normalized.as_str(), "threadid" | "conversationid") {
                        if let Some(thread_id) = scalar_id(value) {
                            output.insert(thread_id);
                        }
                    } else if matches!(normalized.as_str(), "thread" | "conversation")
                        && let Some(thread_id) = value.get("id").and_then(scalar_id)
                    {
                        output.insert(thread_id);
                    }
                    visit(value, output, depth + 1);
                }
            }
            _ => {}
        }
    }

    let mut output = HashSet::new();
    visit(message, &mut output, 0);
    output
}

fn health_envelope(
    epoch: &str,
    status: &str,
    child_exit_code: Option<i32>,
    dropped_control_messages: u64,
) -> Value {
    json!({
        "type": "health",
        "protocol": PROTOCOL_VERSION,
        "engine_epoch": epoch,
        "status": status,
        "accepting_workflow_requests": status == "ready",
        "child_exit_code": child_exit_code,
        "dropped_control_messages": dropped_control_messages,
    })
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

    #[test]
    fn helper_mode_ignores_unrelated_codey_invocations() {
        let parsed = HelperConfig::from_arguments([OsString::from("--codey-record-error")])
            .expect("unrelated helper arguments should be accepted");
        assert!(parsed.is_none());
    }

    #[test]
    fn helper_mode_rejects_recursive_or_non_app_server_invocations() {
        assert!(
            HelperConfig::from_arguments([
                OsString::from(HELPER_ARGUMENT),
                OsString::from(CODEX_EXECUTABLE_ARGUMENT),
                OsString::from("codey"),
                OsString::from("--"),
                OsString::from("app-server"),
            ])
            .is_err()
        );
        assert!(
            HelperConfig::from_arguments([
                OsString::from(HELPER_ARGUMENT),
                OsString::from(CODEX_EXECUTABLE_ARGUMENT),
                OsString::from("codex"),
                OsString::from("--"),
                OsString::from("exec"),
            ])
            .is_err()
        );
    }

    #[test]
    fn workflow_ids_are_namespaced_away_from_electron_numeric_ids() {
        let workflow_id = format_internal_request_id("epoch-a", 42);
        assert_eq!(workflow_id, "codey-workflow:epoch-a:42");
        assert_ne!(Value::String(workflow_id), json!(42));
    }

    #[test]
    fn message_classification_and_thread_discovery_are_protocol_agnostic() {
        assert_eq!(
            message_kind(&json!({"jsonrpc":"2.0","method":"future/method","params":{}})),
            MessageKind::Notification
        );
        assert_eq!(
            message_kind(&json!({"jsonrpc":"2.0","id":7,"method":"future/method"})),
            MessageKind::Request
        );
        assert_eq!(
            message_kind(&json!({"jsonrpc":"2.0","id":7,"result":{}})),
            MessageKind::Response
        );
        assert_eq!(
            extract_thread_ids(&json!({
                "params": {"threadId": "thread-a"},
                "result": {"thread": {"id": "thread-b"}},
            })),
            HashSet::from(["thread-a".to_owned(), "thread-b".to_owned()])
        );
    }

    #[test]
    fn origin_requests_do_not_claim_approval_ownership() {
        let (child_sender, _child_receiver) = mpsc::channel(1);
        let shared = SharedState::new(child_sender);
        let (control_sender, _control_receiver) = mpsc::channel(1);
        let install = shared.install_control(control_sender);
        let request = json!({
            "jsonrpc": "2.0",
            "id": "origin-read",
            "method": "thread/read",
            "params": {"threadId": "origin-thread"},
        });
        let internal_id = shared
            .register_workflow_request(
                install.generation,
                &install.epoch,
                &request,
                json!("origin-read"),
                false,
            )
            .unwrap();
        assert!(shared.routing().workflow_owned_threads.is_empty());

        let mut response = json!({
            "jsonrpc": "2.0",
            "id": internal_id,
            "result": {"thread": {"id": "origin-thread"}},
        });
        assert!(matches!(
            shared.take_internal_response(&mut response),
            InternalResponse::Deliver { .. }
        ));
        assert!(shared.routing().workflow_owned_threads.is_empty());

        let worker_request = json!({
            "jsonrpc": "2.0",
            "id": "worker-start",
            "method": "thread/start",
            "params": {},
        });
        let worker_id = shared
            .register_workflow_request(
                install.generation,
                &install.epoch,
                &worker_request,
                json!("worker-start"),
                true,
            )
            .unwrap();
        let mut worker_response = json!({
            "jsonrpc": "2.0",
            "id": worker_id,
            "result": {"thread": {"id": "worker-thread"}},
        });
        assert!(matches!(
            shared.take_internal_response(&mut worker_response),
            InternalResponse::Deliver { .. }
        ));
        assert!(
            shared
                .routing()
                .workflow_owned_threads
                .contains("worker-thread")
        );
    }

    #[test]
    fn private_control_requires_loopback_and_an_opaque_token() {
        assert!(is_private_control_address(
            "127.0.0.1:43123".parse().unwrap()
        ));
        assert!(is_private_control_address("[::1]:43123".parse().unwrap()));
        assert!(!is_private_control_address(
            "0.0.0.0:43123".parse().unwrap()
        ));
        assert!(!is_private_control_address("127.0.0.1:0".parse().unwrap()));
        assert!(is_valid_capability_token(
            b"abcdefghijklmnopqrstuvwxyzABCDEF"
        ));
        assert!(!is_valid_capability_token(b"short"));
        assert!(!is_valid_capability_token(
            b"this-token-must-never-contain-whitespace "
        ));
        assert!(constant_time_eq(b"same-token", b"same-token"));
        assert!(!constant_time_eq(b"same-token", b"other-token"));
    }

    #[tokio::test]
    async fn jsonl_reader_handles_fragmented_and_coalesced_frames() {
        let (mut writer, reader) = duplex(128);
        let writer_task = tokio::spawn(async move {
            writer.write_all(br#"{"id":1"#).await.unwrap();
            tokio::task::yield_now().await;
            writer
                .write_all(b"}\n{\"method\":\"notice\"}\n")
                .await
                .unwrap();
        });
        let mut reader = BufReader::new(reader);
        assert_eq!(
            read_limited_frame(&mut reader, 128).await.unwrap(),
            Some(b"{\"id\":1}\n".to_vec())
        );
        assert_eq!(
            read_limited_frame(&mut reader, 128).await.unwrap(),
            Some(b"{\"method\":\"notice\"}\n".to_vec())
        );
        writer_task.await.unwrap();
        assert_eq!(read_limited_frame(&mut reader, 128).await.unwrap(), None);
    }
}
