//! 渲染进程早期挂载：浏览器端点上的 Target 自动挂载。
//!
//! Electron 的页面 target 在首个导航提交前可被 `waitForDebuggerOnStart`
//! 暂停：此刻 Fetch 拦截与文档脚本能赶在第一个资产请求之前落位，源码级
//! 请求门禁（app-server 模块改写）在模块求值时即可就绪，不必等整页水合
//! 后走运行时探测。武装失败或错过窗口都安全回退为普通注入路径。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use codey_runtime_core::bridge::{ConnectedCdpSocket, connect_cdp_websocket};
use codey_runtime_core::cdp::{
    CdpTarget, browser_websocket_url, is_primary_codex_page_target, list_targets,
    pick_injectable_codex_page_target,
};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

/// 武装尝试的重试窗口：页面出现得再晚也有机会被暂停；窗口内每次尝试只是
/// loopback 上的几次请求。与 /json 枚举并行跑，不阻塞 Ready 路径。
const ARM_BUDGET: Duration = Duration::from_secs(8);
/// 浏览器端点握手（连接 + setAutoAttach 应答）的单步预算。
const ARM_STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// 放行/清理命令的发送预算：尽力而为，不阻塞注入主流程。
const RELEASE_TIMEOUT: Duration = Duration::from_secs(3);
/// /json 目标枚举的探测间隔。
const TARGET_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const PREFETCH_CLEANUP_BUDGET: Duration = Duration::from_secs(6);

static NEXT_EARLY_ATTACH_ID: AtomicU64 = AtomicU64::new(900_000);

fn next_id() -> u64 {
    NEXT_EARLY_ATTACH_ID.fetch_add(1, Ordering::Relaxed)
}

/// 预取拿到的页面目标：是否带暂停态决定注入路径要不要先放行。
pub enum PrefetchedPage {
    /// 首个导航前已暂停：拦截/文档脚本装好之后必须 resume 才会真正开始加载。
    Paused {
        target: CdpTarget,
        hold: EarlyAttachHold,
    },
    /// 页面已可注入但错过了暂停窗口（武装晚于导航或端点不支持）。
    Ready { target: CdpTarget },
}

#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrefetchDiagnostics {
    outcome: &'static str,
    total_ms: u64,
    wait_ms: u64,
    cleanup_ms: u64,
    cleanup_pending: bool,
    cleanup_confirmed: bool,
    active_operation: Option<&'static str>,
    pub(crate) last_discovery_error: Option<String>,
    last_arm_error: Option<String>,
}

type Diagnostics = Arc<Mutex<PrefetchDiagnostics>>;
type CleanupTasks = Arc<Mutex<Vec<tokio::task::JoinHandle<bool>>>>;

/// 预取任务随调用方一起取消，避免丢弃 JoinHandle 后继续枚举或暂停页面。
pub struct PrefetchTask {
    handle: tokio::task::JoinHandle<Option<PrefetchedPage>>,
    started: tokio::time::Instant,
    diagnostics: Diagnostics,
    cleanup_tasks: CleanupTasks,
}

impl PrefetchTask {
    pub fn abort(&self) {
        self.handle.abort();
    }

    pub(crate) async fn wait(
        mut self,
        deadline: tokio::time::Instant,
        cleanup_deadline: tokio::time::Instant,
    ) -> (Option<PrefetchedPage>, PrefetchDiagnostics) {
        let wait_started = tokio::time::Instant::now();
        let result = match tokio::time::timeout_at(deadline, &mut self.handle).await {
            Ok(result) => result,
            Err(_) => {
                self.diagnostics.lock().unwrap().outcome = "handoffTimeout";
                self.handle.abort();
                // abort 与任务完成可能同时发生；已完成的暂停目标仍交给注入侧。
                (&mut self.handle).await
            }
        };
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                let mut diagnostics = self.diagnostics.lock().unwrap();
                if diagnostics.outcome != "handoffTimeout" {
                    diagnostics.outcome = if error.is_panic() {
                        "panicked"
                    } else {
                        "cancelled"
                    };
                }
                None
            }
        };
        let wait_ms = wait_started.elapsed().as_millis() as u64;
        let cleanup_started = tokio::time::Instant::now();
        // 本体已结束，所有尚未交出的浏览器会话都已登记清理任务。
        let cleanup_tasks = std::mem::take(&mut *self.cleanup_tasks.lock().unwrap());
        let cleanup_result = tokio::time::timeout_at(
            cleanup_deadline,
            futures_util::future::join_all(cleanup_tasks),
        )
        .await;
        let mut diagnostics = self.diagnostics.lock().unwrap().clone();
        diagnostics.total_ms = self.started.elapsed().as_millis() as u64;
        diagnostics.wait_ms = wait_ms;
        diagnostics.cleanup_ms = cleanup_started.elapsed().as_millis() as u64;
        diagnostics.cleanup_pending = cleanup_result.is_err();
        diagnostics.cleanup_confirmed = cleanup_result
            .is_ok_and(|results| results.into_iter().all(|result| matches!(result, Ok(true))));
        if page.is_some() {
            diagnostics.outcome = "ready";
        }
        log_early_attach("early_attach.prefetch_summary", json!(diagnostics));
        (page, diagnostics)
    }
}

impl Drop for PrefetchTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 被暂停主页面的挂载句柄：持有浏览器端点写端与扁平化会话 id。
/// 任何路径上 Drop 都会尽力放行——页面停在暂停态远比丢一次拦截严重；
/// 浏览器会话已断开时改走页面级 WebSocket 兜底放行。
pub struct EarlyAttachHold {
    cleanup: BrowserCleanup,
}

/// 从发送自动挂载命令前就持有清理责任；每个暂停事件都在下一次 await 前登记。
struct BrowserCleanup {
    writer: Option<SplitSink<ConnectedCdpSocket, Message>>,
    reader: Option<SplitStream<ConnectedCdpSocket>>,
    debug_port: u16,
    waiting_sessions: HashMap<String, String>,
    cleanup_tasks: CleanupTasks,
}

impl EarlyAttachHold {
    /// 放行被暂停的页面：恢复等待中的调试器挂起并断开本次自动挂载会话。
    /// 这是尽力而为的发送：目标进程若已退出，发送失败也无副作用。
    pub async fn resume(mut self) {
        if let Some(task) = self.cleanup.release() {
            // 清理在独立任务内完成，调用方取消 resume 也不会中断放行。
            let _ = task.await;
        }
    }
}

impl BrowserCleanup {
    fn release(&mut self) -> Option<tokio::sync::oneshot::Receiver<()>> {
        let writer = self.writer.take()?;
        let reader = self.reader.take()?;
        let mut waiting_sessions = std::mem::take(&mut self.waiting_sessions);
        let debug_port = self.debug_port;
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let (finished, receiver) = tokio::sync::oneshot::channel();
        let task = runtime.spawn(async move {
            let mut socket = writer.reunite(reader).expect("matching browser socket halves");
            let released = tokio::time::timeout(RELEASE_TIMEOUT, async {
                let mut pending = std::collections::HashSet::new();
                for session_id in waiting_sessions.keys() {
                    let id = next_id();
                    pending.insert(id);
                    socket.send(Message::Text(json!({
                        "sessionId": session_id,
                        "id": id,
                        "method": "Runtime.runIfWaitingForDebugger",
                    }).to_string().into())).await?;
                }
                let disable = disable_auto_attach();
                pending.insert(disable["id"].as_u64().expect("command id"));
                socket.send(Message::Text(disable.to_string().into())).await?;
                while !pending.is_empty() {
                    let Some(message) = socket.next().await else {
                        anyhow::bail!("browser cleanup connection closed");
                    };
                    let Message::Text(text) = message? else { continue };
                    let payload: Value = serde_json::from_str(&text)?;
                    if let Some(id) = payload["id"].as_u64()
                        && pending.remove(&id)
                        && payload.get("error").is_some()
                    {
                        anyhow::bail!("browser cleanup command rejected");
                    }
                    // 关闭自动挂载前仍可能收到新暂停事件，清理也负责放行。
                    if payload["method"] == "Target.attachedToTarget"
                        && payload["params"]["waitingForDebugger"] == true
                        && let (Some(session), Some(target)) = (
                            payload["params"]["sessionId"].as_str(),
                            payload["params"]["targetInfo"]["targetId"].as_str(),
                        )
                    {
                        waiting_sessions.insert(session.to_string(), format!("ws://localhost:{debug_port}/devtools/page/{target}"));
                        let id = next_id();
                        pending.insert(id);
                        socket.send(Message::Text(json!({
                            "id":id, "sessionId":session, "method":"Runtime.runIfWaitingForDebugger",
                        }).to_string().into())).await?;
                    }
                }
                socket.close(None).await?;
                Ok::<_, anyhow::Error>(())
            }).await;
            drop(socket);
            let browser_confirmed = matches!(released, Ok(Ok(())));
            let confirmed = if browser_confirmed {
                true
            } else {
                let fallback = tokio::time::timeout(RELEASE_TIMEOUT, futures_util::future::join_all(
                    waiting_sessions.values().map(|url| resume_paused_target_via_page_ws(url))
                )).await;
                !waiting_sessions.is_empty() && fallback.is_ok_and(|results| results.into_iter().all(|confirmed| confirmed))
            };
            log_early_attach("early_attach.cleanup_summary", json!({
                "browserConfirmed": browser_confirmed,
                "resumeConfirmed": confirmed,
                "pausedTargets": waiting_sessions.len(),
            }));
            let _ = finished.send(());
            confirmed
        });
        self.cleanup_tasks.lock().unwrap().push(task);
        Some(receiver)
    }
}

impl Drop for BrowserCleanup {
    fn drop(&mut self) {
        self.release();
    }
}

fn disable_auto_attach() -> Value {
    json!({
        "id": next_id(),
        "method": "Target.setAutoAttach",
        "params": {"autoAttach": false, "waitForDebuggerOnStart": false, "flatten": true},
    })
}

/// 浏览器会话已不可用时，用目标自己的页面级 WebSocket 补发恢复命令：
/// `Runtime.runIfWaitingForDebugger` 是 target 级状态，任意已挂载会话
/// 都能放行。
async fn resume_paused_target_via_page_ws(target_ws_url: &str) -> bool {
    tokio::time::timeout(RELEASE_TIMEOUT, async {
        let mut socket = connect_cdp_websocket(target_ws_url).await?;
        let id = next_id();
        let resume = json!({
            "id": id,
            "method": "Runtime.runIfWaitingForDebugger",
        });
        socket
            .send(Message::Text(resume.to_string().into()))
            .await?;
        while let Some(message) = socket.next().await {
            let Message::Text(text) = message? else {
                continue;
            };
            let payload: Value = serde_json::from_str(&text)?;
            if payload["id"] == id {
                anyhow::ensure!(payload.get("error").is_none(), "page resume rejected");
                let _ = socket.close(None).await;
                return Ok::<_, anyhow::Error>(());
            }
        }
        anyhow::bail!("page resume connection closed")
    })
    .await
    .is_ok_and(|result| result.is_ok())
}

struct BrowserArm {
    cleanup: BrowserCleanup,
    pending_resumes: HashMap<u64, String>,
}

impl BrowserArm {
    async fn confirm_resume(&mut self, payload: &Value) {
        let Some(session_id) = payload["id"]
            .as_u64()
            .and_then(|id| self.pending_resumes.remove(&id))
        else {
            return;
        };
        if payload.get("error").is_some() {
            return;
        }
        // 收到恢复确认后再移除清理记录；拒绝或断线时仍由清理流程重试。
        self.cleanup.waiting_sessions.remove(&session_id);
        let _ = self
            .send(json!({
                "id": next_id(),
                "method": "Target.detachFromTarget",
                "params": {"sessionId": session_id},
            }))
            .await;
    }

    async fn send(&mut self, payload: Value) -> Result<()> {
        writer_send(
            self.cleanup.writer.as_mut().expect("browser writer exists"),
            payload,
        )
        .await
    }

    fn remember_paused(&mut self, debug_port: u16, payload: &Value) {
        if payload["method"] != "Target.attachedToTarget"
            || payload["params"]["waitingForDebugger"] != true
        {
            return;
        }
        if let (Some(session), Some(target)) = (
            payload["params"]["sessionId"].as_str(),
            payload["params"]["targetInfo"]["targetId"].as_str(),
        ) {
            self.cleanup.waiting_sessions.insert(
                session.to_string(),
                format!("ws://localhost:{debug_port}/devtools/page/{target}"),
            );
        }
    }
}

async fn writer_send(
    writer: &mut SplitSink<ConnectedCdpSocket, Message>,
    payload: Value,
) -> Result<()> {
    writer
        .send(Message::Text(payload.to_string().into()))
        .await
        .map_err(|error| anyhow::anyhow!("browser endpoint send failed: {error}"))
}

fn log_early_attach(event: &str, detail: Value) {
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(event, detail);
}

/// 武装尝试的结果：服务未就绪可重试；协议拒绝自动挂载就该永久放弃——
/// 反复发送注定失败的命令只会污染诊断。Armed 附带握手窗口内被跳过
/// 的挂载事件：页面创建可能落在应答之前，事件不补处理目标会一直暂停。
enum ArmAttempt {
    Armed(Box<BrowserArm>, Vec<Value>),
    Retry,
    GiveUp,
}

/// 连上浏览器端点并开启扁平化自动挂载（一次尝试）。参数按 Codex 构建的
/// 实测协议面收窄：waitForDebuggerOnStart 是必填字段；filter 会被接受但
/// 静默排除全部 target，不能用——新目标都在事件里按 waitingForDebugger
/// 逐个放行，语义相同。
async fn arm_browser_endpoint(
    debug_port: u16,
    diagnostics: &Diagnostics,
    cleanup_tasks: &CleanupTasks,
) -> ArmAttempt {
    diagnostics.lock().unwrap().active_operation = Some("browserEndpoint");
    let attempt = async {
        let ws_url = browser_websocket_url(debug_port).await?;
        connect_cdp_websocket(&ws_url).await
    };
    let socket = match attempt.await {
        Ok(socket) => socket,
        Err(error) => {
            let mut diagnostics = diagnostics.lock().unwrap();
            diagnostics.active_operation = None;
            diagnostics.last_arm_error = Some(safe_prefetch_error("browserEndpoint", &error));
            return ArmAttempt::Retry;
        }
    };
    diagnostics.lock().unwrap().active_operation = Some("autoAttachHandshake");
    let (writer, reader) = socket.split();
    let mut arm = BrowserArm {
        pending_resumes: HashMap::new(),
        cleanup: BrowserCleanup {
            writer: Some(writer),
            reader: Some(reader),
            debug_port,
            waiting_sessions: HashMap::new(),
            cleanup_tasks: cleanup_tasks.clone(),
        },
    };
    let attach_id = next_id();
    let request = json!({
        "id": attach_id,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": true,
            "flatten": true,
        },
    });
    if arm.send(request).await.is_err() {
        let mut diagnostics = diagnostics.lock().unwrap();
        diagnostics.active_operation = None;
        diagnostics.last_arm_error = Some("autoAttachSendFailed".to_string());
        log_early_attach(
            "early_attach.arm_failed",
            json!({"stage": "autoAttachSend"}),
        );
        return ArmAttempt::Retry;
    }
    let response = wait_for_response(&mut arm, debug_port, attach_id, ARM_STEP_TIMEOUT).await;
    diagnostics.lock().unwrap().active_operation = None;
    match response {
        Some((true, pending)) => {
            log_early_attach("early_attach.armed", json!({}));
            ArmAttempt::Armed(Box::new(arm), pending)
        }
        Some((false, _)) => {
            diagnostics.lock().unwrap().last_arm_error = Some("autoAttachRejected".to_string());
            log_early_attach(
                "early_attach.arm_failed",
                json!({"stage": "autoAttachRejected"}),
            );
            ArmAttempt::GiveUp
        }
        None => {
            diagnostics.lock().unwrap().last_arm_error = Some("autoAttachNoResponse".to_string());
            log_early_attach(
                "early_attach.arm_failed",
                json!({"stage": "autoAttachNoResponse"}),
            );
            ArmAttempt::Retry
        }
    }
}

/// 等一次握手应答：attachedToTarget 事件可能先于应答到达（页面在武装
/// 过程中已创建）——事件不能丢，随应答一并交回主循环处理。返回 None
/// 表示连接/超时失败；(false, ..) 表示协议拒绝了命令。
async fn wait_for_response(
    arm: &mut BrowserArm,
    debug_port: u16,
    expected_id: u64,
    timeout: Duration,
) -> Option<(bool, Vec<Value>)> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut pending = Vec::new();
    loop {
        let message = tokio::time::timeout_at(deadline, arm.cleanup.reader.as_mut()?.next())
            .await
            .ok()??;
        let Ok(Message::Text(text)) = message else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if payload.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return Some((payload.get("error").is_none(), pending));
        }
        if payload.get("method").and_then(Value::as_str) == Some("Target.attachedToTarget") {
            arm.remember_paused(debug_port, &payload);
            pending.push(payload);
        }
    }
}

/// 主页面判定与运行期 picker 同源：attachedToTarget 的 targetInfo 里不带
/// webSocketDebuggerUrl，按同一套规则构造等价的 CdpTarget 再判定。
fn is_primary_target_info(target_info: &Value) -> bool {
    let target = CdpTarget {
        id: target_info
            .get("targetId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        target_type: target_info
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        title: target_info
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        url: target_info
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        web_socket_debugger_url: Some("ws://localhost/devtools/page/placeholder".to_string()),
    };
    is_primary_codex_page_target(&target)
}

/// 启动预取：浏览器端点自动挂载与 /json 枚举并行。页面在挂载生效后创建会被
/// 暂停（拦截装全由注入侧放行）；页面先行出现就回退为常规目标。
pub fn spawn_early_attach_prefetch(debug_port: u16, budget: Duration) -> PrefetchTask {
    let started = tokio::time::Instant::now();
    let diagnostics = Diagnostics::default();
    let cleanup_tasks = CleanupTasks::default();
    let task_diagnostics = diagnostics.clone();
    let task_cleanup = cleanup_tasks.clone();
    let handle = tokio::spawn(async move {
        let deadline = started + budget;
        let page = tokio::time::timeout_at(
            deadline,
            run_early_attach_prefetch(debug_port, deadline, &task_diagnostics, &task_cleanup),
        )
        .await
        .ok()
        .flatten();
        task_diagnostics.lock().unwrap().outcome = if page.is_some() {
            "ready"
        } else {
            "deadlineExceeded"
        };
        page
    });
    PrefetchTask {
        handle,
        started,
        diagnostics,
        cleanup_tasks,
    }
}

async fn run_early_attach_prefetch(
    debug_port: u16,
    deadline: tokio::time::Instant,
    diagnostics: &Diagnostics,
    cleanup_tasks: &CleanupTasks,
) -> Option<PrefetchedPage> {
    // 武装与枚举并行：devtools 服务就绪时间不可控，两者谁先有结果谁赢。
    // 武装重试只保留一段预算——页面创建早于武装完成就没必要再试。
    let arm_deadline = (tokio::time::Instant::now() + ARM_BUDGET).min(deadline);
    let mut arm: Option<BrowserArm> = None;
    let mut arm_disabled = false;
    let mut next_poll = tokio::time::Instant::now();
    let mut next_arm_attempt = tokio::time::Instant::now();
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            biased;
            message = async {
                match arm.as_mut() {
                    Some(arm) => arm.cleanup.reader.as_mut().expect("browser reader exists").next().await,
                    None => std::future::pending().await,
                }
            } => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(payload) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        if let Some(arm) = arm.as_mut() {
                            arm.remember_paused(debug_port, &payload);
                            arm.confirm_resume(&payload).await;
                        }
                        if payload.get("method").and_then(Value::as_str)
                            == Some("Target.attachedToTarget")
                            && let Some(result) =
                                handle_attached(&mut arm, debug_port, &payload).await
                        {
                            return Some(result);
                        }
                    }
                    Some(Ok(_)) => {}
                    // 浏览器会话断开：武装状态整体作废，直接回退为 /json 枚举。
                    Some(Err(_)) | None => {
                        drop(arm.take());
                        return poll_targets_until_deadline(debug_port, deadline, diagnostics).await;
                    }
                }
            }
            _ = tokio::time::sleep_until(next_poll) => {
                if let Some(target) = poll_targets_once(debug_port, diagnostics).await {
                    return Some(PrefetchedPage::Ready { target });
                }
                next_poll = tokio::time::Instant::now() + TARGET_POLL_INTERVAL;
            }
            _ = tokio::time::sleep_until(next_arm_attempt), if arm.is_none() && !arm_disabled => {
                if tokio::time::Instant::now() >= arm_deadline {
                    arm_disabled = true;
                    continue;
                }
                match arm_browser_endpoint(debug_port, diagnostics, cleanup_tasks).await {
                    ArmAttempt::Armed(session, pending) => {
                        arm = Some(*session);
                        for payload in pending {
                            if let Some(result) =
                                handle_attached(&mut arm, debug_port, &payload).await
                            {
                                return Some(result);
                            }
                        }
                    }
                    ArmAttempt::Retry => {
                        next_arm_attempt =
                            tokio::time::Instant::now() + TARGET_POLL_INTERVAL;
                    }
                    ArmAttempt::GiveUp => arm_disabled = true,
                }
            }
        }
    }
    None
}

/// 处理一次挂载事件。主页面被真正暂停时持有它并解除自动挂载——后续窗口
/// （avatar-overlay、detached-window 等）不再进入暂停；其余被暂停的
/// target 立即放行，不拖累 Codex 自己的启动。
async fn handle_attached(
    arm: &mut Option<BrowserArm>,
    debug_port: u16,
    payload: &Value,
) -> Option<PrefetchedPage> {
    let params = payload.get("params")?;
    let session_id = params.get("sessionId").and_then(Value::as_str)?.to_string();
    let target_info = params.get("targetInfo")?;
    let target_id = target_info
        .get("targetId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let url = target_info
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let waiting = params
        .get("waitingForDebugger")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // 暂停发生在首个导航提交之前：此刻 targetInfo.url 必然为空，按 URL 判主
    // 页面永远失败。启动预取窗口里第一个被暂停的 page 即主窗口（其余窗口都是
    // 挂载后由用户动作触发），URL 未提交的暂停 page 直接视为主目标。
    let unnavigated = url.is_empty() || url == "about:blank";
    let primary = is_primary_target_info(target_info)
        || (waiting
            && unnavigated
            && target_info.get("type").and_then(Value::as_str) == Some("page"));
    log_early_attach(
        "early_attach.attached",
        json!({
            "targetId": target_id,
            "type": target_info.get("type").and_then(Value::as_str),
            "title": target_info.get("title").and_then(Value::as_str),
            "url": url.split('?').next().unwrap_or_default(),
            "waiting": waiting,
            "primary": primary,
            "targetInfo": target_info,
        }),
    );
    if primary && waiting {
        let arm = arm.take()?;
        // 先把自动挂载关掉再交还写端：晚到的窗口不再被暂停，resume 之后
        // 这条浏览器会话只为当前持有服务。本构建的 setAutoAttach 要求
        // waitForDebuggerOnStart 必填，关闭时也要带上。
        let mut hold = EarlyAttachHold {
            cleanup: arm.cleanup,
        };
        let _ = writer_send(
            hold.cleanup.writer.as_mut().expect("browser writer exists"),
            disable_auto_attach(),
        )
        .await;
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "early_attach.paused",
            json!({
                "targetId": target_id,
                "url": url,
            }),
        );
        let target_ws_url = format!("ws://localhost:{debug_port}/devtools/page/{target_id}");
        return Some(PrefetchedPage::Paused {
            target: CdpTarget {
                id: target_id,
                target_type: "page".to_string(),
                title: target_info
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                url,
                web_socket_debugger_url: Some(target_ws_url.clone()),
            },
            hold,
        });
    }
    // 非主目标：暂停的先放行再断开会话；未暂停的（武装前已存在）直接断开，
    // 避免无关会话拖长这条 ws 的事件流。
    if let Some(arm) = arm.as_mut() {
        if waiting {
            let id = next_id();
            arm.pending_resumes.insert(id, session_id.clone());
            if arm
                .send(json!({
                    "sessionId": session_id,
                    "id": id,
                    "method": "Runtime.runIfWaitingForDebugger",
                }))
                .await
                .is_err()
            {
                arm.pending_resumes.remove(&id);
            }
        } else {
            let _ = arm
                .send(json!({
                    "id": next_id(),
                    "method": "Target.detachFromTarget",
                    "params": {"sessionId": session_id},
                }))
                .await;
        }
    }
    None
}

async fn poll_targets_once(debug_port: u16, diagnostics: &Diagnostics) -> Option<CdpTarget> {
    diagnostics.lock().unwrap().active_operation = Some("listTargets");
    let result = list_targets(debug_port).await;
    diagnostics.lock().unwrap().active_operation = None;
    match result {
        Ok(targets) => match pick_injectable_codex_page_target(&targets) {
            Ok(target) => Some(target),
            Err(_) => {
                diagnostics.lock().unwrap().last_discovery_error =
                    Some(format!("noMatchingPage: targetCount={}", targets.len()));
                None
            }
        },
        Err(error) => {
            diagnostics.lock().unwrap().last_discovery_error =
                Some(safe_prefetch_error("listTargets", &error));
            None
        }
    }
}

/// 浏览器会话不可用时的纯枚举回退：与 spawn_injectable_target_prefetch
/// 的旧语义一致。
async fn poll_targets_until_deadline(
    debug_port: u16,
    deadline: tokio::time::Instant,
    diagnostics: &Diagnostics,
) -> Option<PrefetchedPage> {
    while tokio::time::Instant::now() < deadline {
        if let Some(target) = poll_targets_once(debug_port, diagnostics).await {
            return Some(PrefetchedPage::Ready { target });
        }
        tokio::time::sleep(TARGET_POLL_INTERVAL).await;
    }
    None
}

/// 仅保留传输分类和状态码，端点响应正文、页面地址及凭据不进入预取诊断。
fn safe_prefetch_error(stage: &str, error: &anyhow::Error) -> String {
    let message = format!("{error:#}").to_ascii_lowercase();
    let mut reasons = Vec::new();
    for (needle, reason) in [
        ("timed out", "timeout"),
        ("timeout", "timeout"),
        ("connection refused", "connectionRefused"),
        ("10061", "connectionRefused"),
        ("10060", "timeout"),
        ("connection reset", "connectionReset"),
        ("dns", "nameResolution"),
        ("decode", "invalidResponse"),
        ("decoding", "invalidResponse"),
        ("websocketdebuggerurl", "missingWebSocketUrl"),
    ] {
        if message.contains(needle) && !reasons.contains(&reason.to_string()) {
            reasons.push(reason.to_string());
        }
    }
    for status in 100..=599 {
        if message.contains(&format!("({status} ")) {
            reasons.push(format!("httpStatus={status}"));
        }
    }
    if reasons.is_empty() {
        reasons.push("endpointRequestFailed".to_string());
    }
    format!("{stage}: {}", reasons.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn task_from_handle(handle: tokio::task::JoinHandle<Option<PrefetchedPage>>) -> PrefetchTask {
        PrefetchTask {
            handle,
            started: tokio::time::Instant::now(),
            diagnostics: Diagnostics::default(),
            cleanup_tasks: CleanupTasks::default(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pending_prefetch_stops_at_handoff_deadline() {
        let (stopped, stopped_rx) = tokio::sync::oneshot::channel();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let task = task_from_handle(tokio::spawn(async move {
            let _guard = NotifyOnDrop(Some(stopped));
            let _ = ready.send(());
            std::future::pending().await
        }));
        ready_rx.await.unwrap();
        let started = tokio::time::Instant::now();
        let (page, diagnostics) = task
            .wait(
                started + Duration::from_secs(8),
                started + Duration::from_secs(14),
            )
            .await;
        assert!(page.is_none());
        assert_eq!(started.elapsed(), Duration::from_secs(8));
        assert_eq!(diagnostics.outcome, "handoffTimeout");
        assert!(!diagnostics.cleanup_pending);
        stopped_rx.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_prefetch_cancels_the_running_task() {
        let (stopped, stopped_rx) = tokio::sync::oneshot::channel();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let task = task_from_handle(tokio::spawn(async move {
            let _guard = NotifyOnDrop(Some(stopped));
            let _ = ready.send(());
            std::future::pending().await
        }));
        ready_rx.await.unwrap();
        drop(task);
        tokio::time::timeout(Duration::from_secs(1), stopped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn completed_target_is_preserved_at_an_expired_deadline() {
        let task = task_from_handle(tokio::spawn(async {
            Some(PrefetchedPage::Ready {
                target: CdpTarget {
                    id: "ready-target".to_string(),
                    target_type: "page".to_string(),
                    title: "Codex".to_string(),
                    url: "app://-/index.html".to_string(),
                    web_socket_debugger_url: Some("ws://localhost/devtools/page/ready".to_string()),
                },
            })
        }));
        tokio::task::yield_now().await;
        assert!(task.handle.is_finished());
        let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
        let (page, diagnostics) = task.wait(deadline, deadline).await;
        assert!(
            matches!(page, Some(PrefetchedPage::Ready { target }) if target.id == "ready-target")
        );
        assert_eq!(diagnostics.outcome, "ready");
    }

    #[tokio::test]
    async fn cancelled_handshake_releases_paused_page_before_handoff() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = socket.next().await.unwrap().unwrap();
            let _: Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
            socket
                .send(Message::Text(
                    json!({"method":"Target.attachedToTarget", "params":{
                        "sessionId":"paused-session", "waitingForDebugger":true,
                        "targetInfo":{"targetId":"paused-target", "type":"page", "url":""}
                    }})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            // Pong 证明客户端已消费前面的暂停事件；原始握手应答保持未返回。
            socket.send(Message::Ping(Vec::new().into())).await.unwrap();
            let mut ready = Some(ready);
            let mut methods = Vec::new();
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Pong(_)) {
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(());
                    }
                    continue;
                }
                let Message::Text(text) = message else { break };
                let request: Value = serde_json::from_str(&text).unwrap();
                methods.push(request["method"].as_str().unwrap().to_string());
                tokio::time::sleep(Duration::from_millis(20)).await;
                socket
                    .send(Message::Text(
                        json!({"id":request["id"], "result":{}}).to_string().into(),
                    ))
                    .await
                    .unwrap();
            }
            methods
        });
        let socket = connect_cdp_websocket(&format!("ws://{address}"))
            .await
            .unwrap();
        let (writer, reader) = socket.split();
        let cleanup_tasks = CleanupTasks::default();
        let mut arm = BrowserArm {
            pending_resumes: HashMap::new(),
            cleanup: BrowserCleanup {
                writer: Some(writer),
                reader: Some(reader),
                debug_port: address.port(),
                waiting_sessions: HashMap::new(),
                cleanup_tasks: cleanup_tasks.clone(),
            },
        };
        let mut task = task_from_handle(tokio::spawn(async move {
            arm.send(json!({"id":1, "method":"Target.setAutoAttach"}))
                .await
                .unwrap();
            let _ = wait_for_response(&mut arm, address.port(), 1, Duration::from_secs(10)).await;
            panic!("prefetch should be cancelled while awaiting the handshake response");
        }));
        task.cleanup_tasks = cleanup_tasks;
        tokio::time::timeout(Duration::from_secs(1), ready_rx)
            .await
            .unwrap()
            .unwrap();
        let now = tokio::time::Instant::now();
        let (page, diagnostics) = task.wait(now, now + Duration::from_secs(1)).await;
        assert!(page.is_none());
        assert!(!diagnostics.cleanup_pending);
        assert!(diagnostics.cleanup_confirmed);
        assert!(now.elapsed() >= Duration::from_millis(40));
        let methods = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            methods,
            ["Runtime.runIfWaitingForDebugger", "Target.setAutoAttach",]
        );
    }

    #[tokio::test]
    async fn rejected_resume_remains_registered_for_cleanup() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut methods = Vec::new();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                methods.push(request["method"].as_str().unwrap().to_string());
                let response = if methods.len() == 1 {
                    json!({"id": request["id"], "error": {"code": -32000, "message": "try again"}})
                } else {
                    json!({"id": request["id"], "result": {}})
                };
                socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .unwrap();
            }
            methods
        });
        let socket = connect_cdp_websocket(&format!("ws://{address}"))
            .await
            .unwrap();
        let (writer, reader) = socket.split();
        let cleanup_tasks = CleanupTasks::default();
        let mut arm = Some(BrowserArm {
            pending_resumes: HashMap::new(),
            cleanup: BrowserCleanup {
                writer: Some(writer),
                reader: Some(reader),
                debug_port: address.port(),
                waiting_sessions: HashMap::new(),
                cleanup_tasks: cleanup_tasks.clone(),
            },
        });
        let attached = json!({"method": "Target.attachedToTarget", "params": {
            "sessionId": "other-session", "waitingForDebugger": true,
            "targetInfo": {"targetId": "other-target", "type": "worker", "url": "app://-/worker.js"}
        }});
        arm.as_mut()
            .unwrap()
            .remember_paused(address.port(), &attached);
        assert!(
            handle_attached(&mut arm, address.port(), &attached)
                .await
                .is_none()
        );
        let active = arm.as_mut().unwrap();
        assert!(
            active
                .cleanup
                .waiting_sessions
                .contains_key("other-session")
        );
        let message = tokio::time::timeout(
            Duration::from_secs(1),
            active.cleanup.reader.as_mut().unwrap().next(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let rejected: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        active.confirm_resume(&rejected).await;
        assert!(active.pending_resumes.is_empty());
        assert!(
            active
                .cleanup
                .waiting_sessions
                .contains_key("other-session")
        );
        drop(arm);
        let tasks = std::mem::take(&mut *cleanup_tasks.lock().unwrap());
        for task in tasks {
            assert!(
                tokio::time::timeout(Duration::from_secs(1), task)
                    .await
                    .unwrap()
                    .unwrap()
            );
        }
        let methods = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            methods,
            [
                "Runtime.runIfWaitingForDebugger",
                "Runtime.runIfWaitingForDebugger",
                "Target.setAutoAttach",
            ]
        );
    }

    #[tokio::test]
    async fn http_errors_are_diagnostic_without_response_body_or_urls() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 12\r\nConnection: close\r\n\r\nsecret-token").await.unwrap();
            }
        });
        let diagnostics = Diagnostics::default();
        assert!(poll_targets_once(port, &diagnostics).await.is_none());
        server.abort();
        let detail = diagnostics
            .lock()
            .unwrap()
            .last_discovery_error
            .clone()
            .unwrap();
        assert!(detail.contains("httpStatus=503"), "{detail}");
        assert!(!detail.contains("secret-token"));
        assert!(!detail.contains("localhost"));
        assert!(!detail.contains(&port.to_string()));
    }

    #[test]
    fn prefetch_error_summary_never_includes_endpoint_payloads() {
        let error = anyhow::anyhow!(
            "HTTP status client error (403 Forbidden) for url (http://user:secret@localhost/json?token=private); body=personal data"
        );
        assert_eq!(
            safe_prefetch_error("listTargets", &error),
            "listTargets: httpStatus=403"
        );
    }
}
