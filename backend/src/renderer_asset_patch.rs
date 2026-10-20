//! 渲染进程资产补丁的 CDP Fetch 会话。
//!
//! Electron fuse 关闭后主进程 `protocol.handle` 改写不可用；这里用一条独立的
//! page 级 CDP WebSocket 开启 `Fetch` 域拦截，把 `app://` 协议响应的源文本送进
//! 页面里的 `__codeyRendererPatchEntry` 做纯函数改写，再 fulfill 回去。会话随
//! target 存活：页面导航不会打断它，后续文档的资产同样经过拦截，因此同一
//! target 上只需要一次补装式 reload。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::error_log;

/// 补丁脚本注入页面后暴露的纯函数入口：`(url, source) => patched | null`。
const RENDERER_ASSET_PATCH_SCRIPT: &str = include_str!("renderer_asset_patch.js");
/// 注入脚本侧失败日志的回传 binding，由补丁会话独占，不经过会话桥。
const PATCH_LOG_BINDING: &str = "__codeyPatchLog";
const PATCH_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// 资产改写需要的执行上下文若迟迟不出现（例如首导航尚未提交），放行资产比
/// 无限阻塞页面加载更安全——请求入口校验决定这次注入是否算数。
const CONTEXT_WAIT_BUDGET: Duration = Duration::from_secs(20);
const PAUSED_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const BOOTSTRAP_WAIT_BUDGET: Duration = Duration::from_secs(20);
/// 覆盖判定前的上下文探测预算：只决定要不要补装式 reload，等待过久会推迟
/// bridge 安装；超时按"不重载"放行，惰性安装兜底。
const CONTEXT_PROBE_BUDGET: Duration = Duration::from_secs(8);
/// 交给 Fetch 域的 URL 过滤：`app:` 是 Codex 的自定义 bundle 协议，其他
/// scheme 一律不暂停。
const FETCH_URL_PATTERN: &str = "app://*/*";
/// 并发处理暂停请求的上限：资产改写走页面 evaluate，串行反而拖慢首屏；
/// 放开少量并发可以把几十个资产请求的往返交叠起来。
const MAX_CONCURRENT_PAUSES: usize = 4;

static NEXT_PATCH_MESSAGE_ID: AtomicU64 = AtomicU64::new(50_000);

fn next_message_id() -> u64 {
    NEXT_PATCH_MESSAGE_ID.fetch_add(1, Ordering::Relaxed)
}

/// 按 WebSocket 地址缓存补丁会话：注入重试与看门狗重建 bridge 时复用，避免在
/// 同一 target 上叠出第二个 Fetch 拦截器（两个会话都 enable 时暂停事件归属
/// 不确定，资产会被挂住）。
///
/// 缓存必须持有强引用：调用方只在安装那一轮借用会话，注入重试会再次进入安装，
/// 若这里是 `Weak` 则升级必然失败并重建第二个拦截器，旧泵又因自持
/// `command_tx` 而不会退出。泵退出时按指针比对移除自己的条目。
static PATCH_SESSIONS: OnceLock<Mutex<HashMap<String, Arc<RendererAssetPatchSession>>>> =
    OnceLock::new();

/// 已经覆盖过一次「拦截完整加载」的 target：会话活着时新文档的资产都会被拦
/// 截，注入重试不必再为它 reload。会话泵退出时清掉，下一次建立重新判定。
static COVERED_TARGETS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// install 跨 await 串行化：两次并发安装会在同一 target 上得到两个 Fetch
/// 会话。
static INSTALL_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// 补丁会话注册的文档脚本标识（按 target WebSocket 地址）。文档脚本随 target
/// 存活而不随会话消亡，新会话接管同一 target 时必须先清掉旧注册，否则每次
/// 重建都会让补丁脚本在新文档里多跑一遍。
static PATCH_DOC_SCRIPTS: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();

fn patch_doc_scripts() -> &'static Mutex<HashMap<String, Vec<String>>> {
    PATCH_DOC_SCRIPTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn patch_sessions() -> &'static Mutex<HashMap<String, Arc<RendererAssetPatchSession>>> {
    PATCH_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn covered_targets() -> &'static Mutex<HashSet<String>> {
    COVERED_TARGETS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 补丁求值器：补丁是纯函数，只需要一个稳定的 JS 上下文。被拦页面在导航
/// 窗口内的 default 上下文会被 Runtime agent 拒绝执行（"Cannot find
/// context"），暂停等待本身又会拉长该窗口；改到同一调试实例上的常驻辅助
/// 页面（detached-window / avatar-overlay）求值，与被拦页面的导航状态
/// 彻底解耦。
pub(crate) struct PatchEvaluator {
    own_ws_url: String,
    install_script: Arc<str>,
    connection: tokio::sync::Mutex<Option<EvaluatorConn>>,
}

struct EvaluatorConn {
    command_tx: mpsc::UnboundedSender<SessionCommand>,
    pump: tokio::task::JoinHandle<()>,
}

impl PatchEvaluator {
    fn new(own_ws_url: &str, install_script: Arc<str>) -> Arc<Self> {
        Arc::new(Self {
            own_ws_url: own_ws_url.to_string(),
            install_script,
            connection: tokio::sync::Mutex::new(None),
        })
    }

    /// 候选辅助页面每次连接时重新发现：补丁会话建立通常早于 detached-window
    /// 等常驻页面出现，只在安装时枚举会永远拿不到辅助页。
    async fn ensure_connection(&self) -> Result<()> {
        let mut slot = self.connection.lock().await;
        if slot.as_ref().is_some_and(|conn| !conn.pump.is_finished()) {
            return Ok(());
        }
        *slot = None;
        for ws_url in aux_evaluator_ws_candidates(&self.own_ws_url).await {
            let Ok(socket) = codey_runtime_core::bridge::connect_cdp_websocket(&ws_url).await
            else {
                continue;
            };
            let (writer, reader) = socket.split();
            let (command_tx, command_rx) = mpsc::unbounded_channel::<SessionCommand>();
            let pending = Arc::new(Mutex::new(
                HashMap::<u64, oneshot::Sender<CommandResult>>::new(),
            ));
            let pump = tokio::spawn(run_evaluator_pump(writer, reader, command_rx, pending));
            *slot = Some(EvaluatorConn { command_tx, pump });
            return Ok(());
        }
        bail!("没有可用的辅助页面求值目标")
    }

    /// 在辅助页 default 上下文里跑补丁入口；入口缺失时在同一表达式里就地
    /// 安装（幂等），与页面侧惰性安装同一形态。
    async fn evaluate(&self, url: &str, source: &str) -> Result<Option<String>> {
        let expression = format!(
            "(()=>{{try{{if(typeof globalThis.__codeyRendererPatchEntry!==\"function\"){{{install}}};return typeof globalThis.__codeyRendererPatchEntry===\"function\"?globalThis.__codeyRendererPatchEntry({u},{s}):null}}catch(_){{return null}}}})()",
            install = self.install_script,
            u = serde_json::to_string(url).context("补丁 URL 序列化失败")?,
            s = serde_json::to_string(source).context("补丁源码序列化失败")?,
        );
        let mut last_error = None;
        for attempt in 0..2 {
            if let Err(error) = self.ensure_connection().await {
                last_error = Some(error);
                continue;
            }
            let command_tx = {
                let slot = self.connection.lock().await;
                slot.as_ref().map(|conn| conn.command_tx.clone())
            };
            let Some(command_tx) = command_tx else {
                continue;
            };
            let result = send_command_via(
                &command_tx,
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true }),
            )
            .await;
            match result {
                Ok(value) => {
                    return Ok(value
                        .get("result")
                        .and_then(|result| result.get("result"))
                        .and_then(|result| result.get("value"))
                        .and_then(Value::as_str)
                        .map(str::to_string));
                }
                Err(error) => {
                    last_error = Some(error);
                    // 连接级错误（泵退出/命令超时）丢连接重建一次；CDP 业务
                    // 错误没有重试价值。
                    let transport = {
                        let slot = self.connection.lock().await;
                        slot.as_ref().is_none_or(|conn| conn.pump.is_finished())
                    };
                    if !transport || attempt > 0 {
                        break;
                    }
                    *self.connection.lock().await = None;
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("辅助页面补丁求值不可用")))
    }
}

/// 辅助页求值会话的最小泵：只路由命令响应，事件全部忽略。
async fn run_evaluator_pump(
    mut writer: futures_util::stream::SplitSink<
        codey_runtime_core::bridge::ConnectedCdpSocket,
        Message,
    >,
    mut reader: futures_util::stream::SplitStream<codey_runtime_core::bridge::ConnectedCdpSocket>,
    mut command_rx: mpsc::UnboundedReceiver<SessionCommand>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<CommandResult>>>>,
) {
    loop {
        tokio::select! {
            command = command_rx.recv() => {
                let Some(command) = command else { break };
                let SessionCommand::Cdp { method, params, respond } = command else { continue };
                let id = next_message_id();
                pending
                    .lock()
                    .expect("evaluator pending map poisoned")
                    .insert(id, respond);
                let text = json!({ "id": id, "method": method, "params": params }).to_string();
                if writer.send(Message::Text(text.into())).await.is_err() {
                    pending
                        .lock()
                        .expect("evaluator pending map poisoned")
                        .remove(&id);
                    break;
                }
            }
            message = reader.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        if let Some(id) = value.get("id").and_then(Value::as_u64)
                            && let Some(respond) = pending
                                .lock()
                                .expect("evaluator pending map poisoned")
                                .remove(&id)
                        {
                            let _ = respond.send(if value.get("error").is_some() {
                                Err(value.to_string())
                            } else {
                                Ok(value)
                            });
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
    pending
        .lock()
        .expect("evaluator pending map poisoned")
        .drain()
        .for_each(|(_, respond)| {
            let _ = respond.send(Err("evaluator session closed".to_string()));
        });
    let _ = writer.close().await;
}

fn ws_url_port(ws_url: &str) -> Option<u16> {
    ws_url
        .split("://")
        .nth(1)?
        .split(':')
        .nth(1)?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// 选辅助求值页面：优先 detached-window 这类常驻宿主页，其次 avatar
/// overlay，再退回任意其他页面 target；排除被拦页面本身。
async fn aux_evaluator_ws_candidates(own_ws_url: &str) -> Vec<String> {
    let Some(port) = ws_url_port(own_ws_url) else {
        return Vec::new();
    };
    let Ok(targets) = codey_runtime_core::cdp::list_targets(port).await else {
        return Vec::new();
    };
    let mut detached = Vec::new();
    let mut overlay = Vec::new();
    let mut other = Vec::new();
    for target in targets {
        if target.target_type != "page" {
            continue;
        }
        let Some(ws_url) = target.web_socket_debugger_url.filter(|url| !url.is_empty()) else {
            continue;
        };
        if ws_url == own_ws_url {
            continue;
        }
        let url = target.url.to_ascii_lowercase();
        if url.starts_with("app://-/detached-window") {
            detached.push(ws_url);
        } else if url.contains("avatar-overlay") {
            overlay.push(ws_url);
        } else {
            other.push(ws_url);
        }
    }
    [detached, overlay, other].concat()
}

/// 补丁脚本注入页面用的完整文档脚本：先放选项全局，再定义入口。
pub(crate) fn renderer_asset_patch_document_script(disable_pet: bool) -> String {
    let options = serde_json::json!({ "disablePet": disable_pet });
    format!(
        "globalThis.__CODEY_RENDERER_PATCH_OPTIONS__ = {options};\n{RENDERER_ASSET_PATCH_SCRIPT}"
    )
}

type CommandResult = Result<Value, String>;

enum SessionCommand {
    Cdp {
        method: &'static str,
        params: Value,
        respond: oneshot::Sender<CommandResult>,
    },
    /// 只要求把命令发出去：放行暂停资产时不需要结果，走这条路径就可以不 spawn
    /// 任务、也不在 pending 表里留条目。
    CdpFireAndForget { method: &'static str, params: Value },
    /// Runtime 初始上下文可能先于主 frame 查询到达，查询完成后重新选择。
    RefreshMainContext,
}

/// 会话内资产拦截计数。每个暂停任务在独立任务里跑，用原子计数而不是
/// 通道回报，泵退出时也不会有未回收的尾账。
#[derive(Default)]
struct PatchSessionCounters {
    /// requestPaused 总数：非零本身就证明 Fetch 在该构建上能拦到资产流。
    paused: AtomicU64,
    /// 真正跑过补丁入口求值的候选数。
    evaluated: AtomicU64,
    /// 改写后以 fulfillRequest 送回的资产数。
    patched: AtomicU64,
    /// 候选资产被原样放行的次数：求值失败、补丁返回 null 或 fulfill 失败。
    skipped: AtomicU64,
    /// Debugger.scriptParsed 观察到的候选脚本数：解析事件不受自定义协议影响，
    /// 但异步处理事件时脚本可能已经开始求值。
    script_parsed: AtomicU64,
    /// 经 setScriptSource 写回补丁源码的脚本数。
    script_patched: AtomicU64,
    /// scriptParsed 路径里读源/求值/写回任一环失败的次数。
    script_patch_failed: AtomicU64,
    /// 检测到 setScriptSource 被构建期禁用后置位：之后的新脚本直接跳过。
    script_edit_disabled: AtomicU64,
}

/// 补丁会话的拦截统计快照。`entry_documents` 是会话建立后观察到的入口文档
/// 暂停次数——为 0 说明该构建的资产流根本没经过 Fetch 通道。
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PatchSessionStats {
    pub(crate) paused_requests: u64,
    pub(crate) evaluated_assets: u64,
    pub(crate) patched_assets: u64,
    pub(crate) skipped_assets: u64,
    pub(crate) entry_documents: u64,
    pub(crate) script_parsed: u64,
    pub(crate) script_patched: u64,
    pub(crate) script_patch_failed: u64,
    pub(crate) script_edit_disabled: bool,
}

/// 注入侧持有的补丁会话句柄；泵退出后由下一次安装重建。
pub(crate) struct RendererAssetPatchSession {
    websocket_url: Arc<str>,
    command_tx: mpsc::UnboundedSender<SessionCommand>,
    bootstrap_epoch: watch::Sender<u64>,
    main_context: watch::Sender<Option<u64>>,
    counters: Arc<PatchSessionCounters>,
    pump: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RendererAssetPatchSession {
    pub(crate) fn is_alive(&self) -> bool {
        self.pump
            .lock()
            .expect("patch session pump lock poisoned")
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    }

    fn install_pump(&self, pump: tokio::task::JoinHandle<()>) {
        *self.pump.lock().expect("patch session pump lock poisoned") = Some(pump);
    }

    fn command(&self, method: &'static str, params: Value) -> oneshot::Receiver<CommandResult> {
        let (respond, receiver) = oneshot::channel();
        // 泵已退出时 send 失败，respond 随命令一起被丢弃，receiver 自然以
        // RecvError 收尾，调用方按会话不可用处理。
        let _ = self.command_tx.send(SessionCommand::Cdp {
            method,
            params,
            respond,
        });
        receiver
    }

    async fn send_command(&self, method: &'static str, params: Value) -> Result<Value> {
        let receiver = self.command(method, params);
        match tokio::time::timeout(PATCH_COMMAND_TIMEOUT, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => bail!("CDP command {method} failed: {error}"),
            Ok(Err(_)) => bail!("补丁会话已退出，CDP command {method} 无响应"),
            Err(_) => bail!("CDP command {method} timed out"),
        }
    }

    /// 确保当前文档的资产都经过拦截。
    ///
    /// 判据是会话自己的观察结果，而不是页面里有没有补丁入口：`bootstrap` 会在
    /// 当前文档里就地装一次入口，所以「入口存在」对推断覆盖毫无信息量。真正有效
    /// 的证据是会话建立后观察到过 `/index.html` 的暂停——那说明有一个文档从入口
    /// 文档开始完整走过了 Fetch 通道，它的资产都会被改写。
    ///
    /// 因此只有「会话早于当前文档」才需要补装式 reload，且必须等文档完整加载
    /// （readyState=complete）：此时页面处于空闲导航间隙，重载是一次正常的新导航。
    /// 文档仍在 loading 时绝不可 reload——中止进行中的首载导航会让 Codex 的
    /// window-manager 报"Failed to load bundle"并直接退出应用；加载中/无上下文时
    /// 改为放行，暂停资产由惰性安装的补丁入口就地改写。
    pub(crate) async fn ensure_document_coverage(&self) -> Result<()> {
        if covered_targets()
            .lock()
            .expect("covered target registry poisoned")
            .contains(self.websocket_url.as_ref())
        {
            return Ok(());
        }
        if *self.bootstrap_epoch.borrow() > 0 {
            // 会话建立后已经有过入口文档导航：该文档的资产都经过 Fetch 通道。
            self.mark_covered();
            return Ok(());
        }
        let mut context_rx = self.main_context.subscribe();
        let context_id = {
            if context_rx.borrow().is_some() {
                *context_rx.borrow()
            } else {
                // 等一个较短的窗口：上下文还没出现说明首导航尚未提交，资产
                // 请求本身已经过 Fetch 通道，惰性安装会覆盖它们。
                tokio::time::timeout(CONTEXT_PROBE_BUDGET, async {
                    while context_rx.borrow().is_none() {
                        if context_rx.changed().await.is_err() {
                            return None;
                        }
                    }
                    *context_rx.borrow()
                })
                .await
                .ok()
                .flatten()
            }
        };
        let Some(context_id) = context_id else {
            self.mark_covered();
            return Ok(());
        };
        let probe = self
            .send_command(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration":
                        "()=>({ready:document.readyState,url:String(location.href)})",
                    "executionContextId": context_id,
                    "returnByValue": true,
                }),
            )
            .await;
        let document_state = probe.ok().and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.get("result"))
                .and_then(|result| result.get("value"))
                .cloned()
        });
        let loaded_app_document = document_state.as_ref().is_some_and(|state| {
            state.get("ready").and_then(Value::as_str) == Some("complete")
                && state
                    .get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.starts_with("app://"))
        });
        if !loaded_app_document {
            // 文档仍在加载，或探测失败：不动页面。
            self.mark_covered();
            return Ok(());
        }
        let mut epoch = self.bootstrap_epoch.subscribe();
        let before = *epoch.borrow_and_update();
        self.send_command("Page.reload", json!({ "ignoreCache": true }))
            .await
            .context("重新加载 Codex 页面以启用资产拦截失败")?;
        // 等重载后的 index.html 真的经过一次暂停处理：这是「当前文档资产被
        // 拦截覆盖」的证据，而不是 reload 命令本身成功。
        let waited = tokio::time::timeout(BOOTSTRAP_WAIT_BUDGET, async {
            while *epoch.borrow() <= before {
                if epoch.changed().await.is_err() {
                    return Err(());
                }
            }
            Ok::<(), ()>(())
        })
        .await;
        match waited {
            Ok(Ok(())) => {
                self.mark_covered();
                Ok(())
            }
            _ => bail!("等待 Codex 页面重载后的 index.html 拦截超时"),
        }
    }

    fn mark_covered(&self) {
        covered_targets()
            .lock()
            .expect("covered target registry poisoned")
            .insert(self.websocket_url.to_string());
    }

    fn stats(&self) -> PatchSessionStats {
        PatchSessionStats {
            paused_requests: self.counters.paused.load(Ordering::Relaxed),
            evaluated_assets: self.counters.evaluated.load(Ordering::Relaxed),
            patched_assets: self.counters.patched.load(Ordering::Relaxed),
            skipped_assets: self.counters.skipped.load(Ordering::Relaxed),
            entry_documents: *self.bootstrap_epoch.borrow(),
            script_parsed: self.counters.script_parsed.load(Ordering::Relaxed),
            script_patched: self.counters.script_patched.load(Ordering::Relaxed),
            script_patch_failed: self.counters.script_patch_failed.load(Ordering::Relaxed),
            script_edit_disabled: self.counters.script_edit_disabled.load(Ordering::Relaxed) != 0,
        }
    }
}

/// 查询某 renderer target 补丁会话的拦截统计。会话缺失或已退出返回 None：
/// 「没有会话」与「会话没拦到资产」在降级诊断里是两种不同的故障。
pub(crate) fn session_stats(websocket_url: &str) -> Option<PatchSessionStats> {
    patch_sessions()
        .lock()
        .expect("patch session registry poisoned")
        .get(websocket_url)
        .map(|session| session.stats())
}

/// 找到或建立这个 renderer target 的补丁会话。
pub(crate) async fn install_renderer_asset_patch(
    websocket_url: &str,
    disable_pet: bool,
    paused_start: bool,
) -> Result<Arc<RendererAssetPatchSession>> {
    let _install_guard = INSTALL_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if let Some(existing) = patch_sessions()
        .lock()
        .expect("patch session registry poisoned")
        .get(websocket_url)
        .filter(|session| session.is_alive())
        .cloned()
    {
        return Ok(existing);
    }

    let socket = codey_runtime_core::bridge::connect_cdp_websocket(websocket_url)
        .await
        .context("连接渲染资产补丁 CDP WebSocket 失败")?;
    let (writer, reader) = socket.split();
    let (command_tx, command_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let (bootstrap_epoch_tx, _bootstrap_epoch_rx) = watch::channel::<u64>(0);
    let (main_context_tx, _main_context_rx) = watch::channel::<Option<u64>>(None);
    let main_frame_id = Arc::new(Mutex::new(None::<String>));
    let pending = Arc::new(Mutex::new(
        HashMap::<u64, oneshot::Sender<CommandResult>>::new(),
    ));

    let session = Arc::new(RendererAssetPatchSession {
        websocket_url: Arc::from(websocket_url),
        command_tx: command_tx.clone(),
        counters: Arc::new(PatchSessionCounters::default()),
        bootstrap_epoch: bootstrap_epoch_tx.clone(),
        main_context: main_context_tx.clone(),
        pump: Mutex::new(None),
    });

    let install_script: Arc<str> = Arc::from(renderer_asset_patch_document_script(disable_pet));
    let evaluator = PatchEvaluator::new(websocket_url, install_script.clone());
    let pump = tokio::spawn(run_patch_pump(PatchPumpArgs {
        websocket_url: session.websocket_url.clone(),
        session: Arc::downgrade(&session),
        writer,
        reader,
        command_rx,
        command_tx,
        pending,
        main_context_tx,
        main_frame_id: main_frame_id.clone(),
        bootstrap_epoch_tx,
        install_script,
        evaluator,
        counters: session.counters.clone(),
    }));
    session.install_pump(pump);

    // 同步握手失败时回收本轮已 spawn 的泵与 CDP 连接：否则会留下一条永不退出的
    // 常驻任务和一条已 enable 却无人应答的 Fetch 拦截器。被清掉的文档脚本注册
    // 由下一次安装重新登记，不回填。
    if let Err(error) = session
        .bootstrap(disable_pet, &main_frame_id, paused_start)
        .await
    {
        if let Some(pump) = session
            .pump
            .lock()
            .expect("patch session pump lock poisoned")
            .take()
        {
            pump.abort();
        }
        return Err(error);
    }

    patch_sessions()
        .lock()
        .expect("patch session registry poisoned")
        .insert(websocket_url.to_string(), Arc::clone(&session));
    Ok(session)
}

impl RendererAssetPatchSession {
    async fn bootstrap(
        &self,
        disable_pet: bool,
        main_frame_id: &Arc<Mutex<Option<String>>>,
        paused_start: bool,
    ) -> Result<()> {
        self.send_command("Runtime.enable", json!({}))
            .await
            .context("启用补丁会话 Runtime 域失败")?;
        // 补丁日志 binding 先于页面脚本：binding 本身按 target 注册，不随文档
        // 重建丢失。
        self.send_command("Runtime.addBinding", json!({ "name": PATCH_LOG_BINDING }))
            .await
            .context("注册补丁日志 binding 失败")?;
        self.send_command("Page.enable", json!({}))
            .await
            .context("启用补丁会话 Page 域失败")?;
        // 主 frame id 供上下文筛选；拿不到也可以继续（回退取最近的 default
        // 上下文）。
        if let Ok(tree) = self.send_command("Page.getFrameTree", json!({})).await
            && let Some(id) = tree
                .get("result")
                .and_then(|result| result.get("frameTree"))
                .and_then(|frame_tree| frame_tree.get("frame"))
                .and_then(|frame| frame.get("id"))
                .and_then(Value::as_str)
        {
            *main_frame_id.lock().expect("main frame id lock poisoned") = Some(id.to_string());
            let _ = self.command_tx.send(SessionCommand::RefreshMainContext);
        }
        // 补丁脚本走文档级注册：必须在 Fetch.enable 之前落位，这样 reload 后的
        // 新文档在任何资产暂停发生前就有 __codeyRendererPatchEntry 可用。同一
        // target 上旧会话留下的注册先清掉，补丁脚本本身幂等但没必要多跑。
        let stale = patch_doc_scripts()
            .lock()
            .expect("patch document script registry poisoned")
            .remove(self.websocket_url.as_ref())
            .unwrap_or_default();
        for identifier in stale {
            let _ = self
                .send_command(
                    "Page.removeScriptToEvaluateOnNewDocument",
                    json!({ "identifier": identifier }),
                )
                .await;
        }
        let script = renderer_asset_patch_document_script(disable_pet);
        let response = self
            .send_command(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": script }),
            )
            .await
            .context("注册渲染资产补丁文档脚本失败")?;
        if let Some(identifier) = response
            .get("result")
            .and_then(|result| result.get("identifier"))
            .and_then(Value::as_str)
        {
            patch_doc_scripts()
                .lock()
                .expect("patch document script registry poisoned")
                .entry(self.websocket_url.to_string())
                .or_default()
                .push(identifier.to_string());
        }
        self.send_command(
            "Fetch.enable",
            json!({
                "patterns": [{
                    "urlPattern": FETCH_URL_PATTERN,
                    "requestStage": "Response",
                }],
            }),
        )
        .await
        .context("启用渲染资产 Fetch 拦截失败")?;
        // 部分构建的 app:// 资产请求不过 Fetch，scriptParsed 提供备用改写机会。
        // 事件处理与 getScriptSource / setScriptSource 都是异步的，也没有暂停
        // 脚本执行，因此不保证赶在求值前完成，更不能保证避免静态 import 的
        // 副作用。失败不致命：文档脚本与 Fetch 会话照常工作。
        if let Err(error) = self.send_command("Debugger.enable", json!({})).await {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "renderer_asset_patch.debugger_enable_failed",
                json!({
                    "message": format!("{error:#}"),
                    "websocketUrl": self.websocket_url.as_ref(),
                }),
            );
        }
        // 当前文档里立刻装一份补丁入口，覆盖「会话建好但 bridge 文档脚本尚未
        // 落地」这段窗口内到达的暂停请求。失败不致命：文档脚本会在下一次
        // 导航时补装。页面被早期挂载暂停时还没有可求值上下文：求值只会排队到
        // 放行或超时，直接跳过——首个文档将由刚注册的文档脚本覆盖。
        if !paused_start
            && let Err(error) = self
                .send_command(
                    "Runtime.evaluate",
                    codey_runtime_core::bridge::runtime_evaluate_params(&script),
                )
                .await
        {
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "renderer_asset_patch.evaluate_failed",
                json!({
                    "message": format!("{error:#}"),
                    "websocketUrl": self.websocket_url.as_ref(),
                }),
            );
        }
        Ok(())
    }
}

struct PatchPumpArgs {
    websocket_url: Arc<str>,
    /// 宿主会话的弱引用：泵退出时只清掉仍指向自己的缓存条目，避免误删接替者。
    session: Weak<RendererAssetPatchSession>,
    writer:
        futures_util::stream::SplitSink<codey_runtime_core::bridge::ConnectedCdpSocket, Message>,
    reader: futures_util::stream::SplitStream<codey_runtime_core::bridge::ConnectedCdpSocket>,
    command_rx: mpsc::UnboundedReceiver<SessionCommand>,
    command_tx: mpsc::UnboundedSender<SessionCommand>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<CommandResult>>>>,
    main_context_tx: watch::Sender<Option<u64>>,
    main_frame_id: Arc<Mutex<Option<String>>>,
    bootstrap_epoch_tx: watch::Sender<u64>,
    /// 补丁入口缺失时在暂停请求里就地安装的完整脚本（含选项全局）。
    install_script: Arc<str>,
    /// 辅助页求值器：连接惰性建立，找不到辅助 target 时退回被拦页上下文。
    evaluator: Arc<PatchEvaluator>,
    counters: Arc<PatchSessionCounters>,
}

/// 泵退出时清理共享状态：pending 一律以失败收尾，「已覆盖」标记作废——后续
/// 建立必须重新评估这个文档是否需要 reload。
async fn run_patch_pump(args: PatchPumpArgs) {
    let PatchPumpArgs {
        websocket_url,
        session,
        mut writer,
        mut reader,
        mut command_rx,
        command_tx,
        pending,
        main_context_tx,
        main_frame_id,
        bootstrap_epoch_tx,
        install_script,
        evaluator,
        counters,
    } = args;
    let mut contexts: HashMap<u64, ContextInfo> = HashMap::new();
    let mut inflight = tokio::task::JoinSet::new();
    let result: Result<()> = async {
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some(command) = command else { break };
                    let (method, params, respond) = match command {
                        SessionCommand::Cdp { method, params, respond } => (method, params, Some(respond)),
                        SessionCommand::CdpFireAndForget { method, params } => (method, params, None),
                        SessionCommand::RefreshMainContext => {
                            publish_main_context(&contexts, &main_frame_id, &main_context_tx, &counters, &command_tx, false);
                            continue;
                        }
                    };
                    let id = next_message_id();
                    if let Some(respond) = respond {
                        pending
                            .lock()
                            .expect("patch session pending map poisoned")
                            .insert(id, respond);
                    }
                    let text = json!({ "id": id, "method": method, "params": params }).to_string();
                    if let Err(error) = writer.send(Message::Text(text.into())).await {
                        pending
                            .lock()
                            .expect("patch session pending map poisoned")
                            .remove(&id);
                        return Err(error).context("发送补丁会话 CDP 命令失败");
                    }
                }
                _ = inflight.join_next(), if !inflight.is_empty() => {}
                message = reader.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let value: Value = match serde_json::from_str(&text) {
                                Ok(value) => value,
                                Err(error) => {
                                    return Err(error).context("解析补丁会话 CDP 消息失败");
                                }
                            };
                            if let Some(id) = value.get("id").and_then(Value::as_u64) {
                                if let Some(respond) = pending
                                    .lock()
                                    .expect("patch session pending map poisoned")
                                    .remove(&id)
                                {
                                    let _ = respond.send(if value.get("error").is_some() {
                                        Err(value.to_string())
                                    } else {
                                        Ok(value)
                                    });
                                }
                                continue;
                            }
                            handle_patch_event(
                                &value,
                                &mut PatchEventState {
                                    contexts: &mut contexts,
                                    main_frame_id: &main_frame_id,
                                    main_context_tx: &main_context_tx,
                                    bootstrap_epoch_tx: &bootstrap_epoch_tx,
                                    command_tx: &command_tx,
                                    install_script: &install_script,
                                    evaluator: &evaluator,
                                    counters: &counters,
                                    inflight: &mut inflight,
                                },
                            );
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(error)) => {
                            return Err(error).context("读取补丁会话 CDP 消息失败");
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "renderer_asset_patch.pump_failed",
            json!({
                "message": format!("{error:#}"),
                "websocketUrl": websocket_url.as_ref(),
            }),
        );
    }
    // 排空 pending：所有等待中的命令以失败结束；暂停的子任务拿到错误后走
    // continueRequest，尽量不把资产挂住。
    pending
        .lock()
        .expect("patch session pending map poisoned")
        .drain()
        .for_each(|(_, respond)| {
            let _ = respond.send(Err("patch session closed".to_string()));
        });
    // 只清理仍属于本会话的共享状态：注入重试可能已经装上接替自己的新会话，
    // 无条件按 URL 删除会把它的拦截器一并注销。注册表锁必须在 await 之前释放。
    let owns_registration = {
        let mut sessions = patch_sessions()
            .lock()
            .expect("patch session registry poisoned");
        if sessions
            .get(websocket_url.as_ref())
            .is_some_and(|current| Weak::ptr_eq(&session, &Arc::downgrade(current)))
        {
            sessions.remove(websocket_url.as_ref());
            true
        } else {
            false
        }
    };
    if owns_registration {
        covered_targets()
            .lock()
            .expect("covered target registry poisoned")
            .remove(websocket_url.as_ref());
    }
    let _ = writer.close().await;
}

struct ContextInfo {
    frame_id: Option<String>,
    is_default: bool,
}

/// 补丁泵的事件处理上下文：把共享状态打包传递，避免逐个事件展开成九个参数。
struct PatchEventState<'a> {
    contexts: &'a mut HashMap<u64, ContextInfo>,
    main_frame_id: &'a Arc<Mutex<Option<String>>>,
    main_context_tx: &'a watch::Sender<Option<u64>>,
    bootstrap_epoch_tx: &'a watch::Sender<u64>,
    command_tx: &'a mpsc::UnboundedSender<SessionCommand>,
    install_script: &'a Arc<str>,
    evaluator: &'a Arc<PatchEvaluator>,
    counters: &'a Arc<PatchSessionCounters>,
    inflight: &'a mut tokio::task::JoinSet<()>,
}

fn handle_patch_event(value: &Value, state: &mut PatchEventState<'_>) {
    let PatchEventState {
        contexts,
        main_frame_id,
        main_context_tx,
        bootstrap_epoch_tx,
        command_tx,
        install_script,
        evaluator,
        counters,
        inflight,
    } = state;
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return;
    };
    let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "Fetch.requestPaused" => {
            counters.paused.fetch_add(1, Ordering::Relaxed);
            let url = params
                .get("request")
                .and_then(|request| request.get("url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if is_bootstrap_url(&url) {
                // 证据按「看到暂停」即记：即便后续改写失败，这个文档的资产
                // 确实经过了拦截通道。
                bump_bootstrap_epoch(bootstrap_epoch_tx);
            }
            if !is_patch_candidate_url(&url) {
                // 放行不需要结果，直接排队即可：不 spawn 任务、不在 pending
                // 表里留条目。首屏的图片、字体等大量 app:// 请求都走这条路。
                let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
                    return;
                };
                enqueue_continue_request(command_tx, request_id);
                return;
            }
            let context_rx = main_context_tx.subscribe();
            let command_tx = command_tx.clone();
            let install_script = install_script.clone();
            let evaluator = evaluator.clone();
            let counters = counters.clone();
            if inflight.len() < MAX_CONCURRENT_PAUSES {
                inflight.spawn(async move {
                    process_paused_asset(
                        command_tx,
                        context_rx,
                        install_script,
                        evaluator,
                        counters,
                        params,
                    )
                    .await;
                });
            } else {
                // 背压：并发满时仍要回应，直接放行比挂在浏览器侧安全。
                counters.skipped.fetch_add(1, Ordering::Relaxed);
                if let Some(request_id) = params.get("requestId").and_then(Value::as_str) {
                    enqueue_continue_request(&command_tx, request_id);
                }
            }
        }
        "Runtime.executionContextCreated" => {
            let context = params.get("context").cloned().unwrap_or_else(|| json!({}));
            let id = context.get("id").and_then(Value::as_u64).unwrap_or(0);
            let aux = context.get("auxData").cloned().unwrap_or_else(|| json!({}));
            let frame_id = aux
                .get("frameId")
                .and_then(Value::as_str)
                .map(str::to_string);
            let is_default = aux
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && context
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    == "default";
            contexts.insert(
                id,
                ContextInfo {
                    frame_id,
                    is_default,
                },
            );
            publish_main_context(
                contexts,
                main_frame_id,
                main_context_tx,
                counters,
                command_tx,
                false,
            );
        }
        "Runtime.executionContextDestroyed" => {
            if let Some(id) = params.get("executionContextId").and_then(Value::as_u64) {
                contexts.remove(&id);
                publish_main_context(
                    contexts,
                    main_frame_id,
                    main_context_tx,
                    counters,
                    command_tx,
                    false,
                );
            }
        }
        "Runtime.executionContextsCleared" => {
            contexts.clear();
            publish_main_context(
                contexts,
                main_frame_id,
                main_context_tx,
                counters,
                command_tx,
                false,
            );
        }
        "Page.frameNavigated" => {
            let frame = params.get("frame").cloned().unwrap_or_else(|| json!({}));
            if frame.get("parentId").is_none()
                && let Some(id) = frame.get("id").and_then(Value::as_str)
            {
                *main_frame_id.lock().expect("main frame id lock poisoned") = Some(id.to_string());
                publish_main_context(
                    contexts,
                    main_frame_id,
                    main_context_tx,
                    counters,
                    command_tx,
                    true,
                );
            }
        }
        "Runtime.bindingCalled"
            if params.get("name").and_then(Value::as_str) == Some(PATCH_LOG_BINDING) =>
        {
            record_patch_log(params.get("payload").and_then(Value::as_str));
        }
        "Debugger.scriptParsed" => {
            let url = params
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // 只处理静态名单内的可补丁脚本：getScriptSource 会把整个模块正文
            // （app-shared 约 8MB）经 CDP 拉回来，不过滤就是几百次无谓往返。
            if !is_patch_candidate_url(url) || !is_patchable_named_asset(url) {
                return;
            }
            // live-edit 被构建期禁用的 Electron 上 setScriptSource 恒失败；
            // 熔断后不再为每个新脚本重复取源求值。
            if counters.script_edit_disabled.load(Ordering::Relaxed) != 0 {
                return;
            }
            let Some(script_id) = params
                .get("scriptId")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return;
            };
            counters.script_parsed.fetch_add(1, Ordering::Relaxed);
            if inflight.len() < MAX_CONCURRENT_PAUSES {
                inflight.spawn(handle_parsed_script(
                    command_tx.clone(),
                    main_context_tx.subscribe(),
                    install_script.clone(),
                    evaluator.clone(),
                    counters.clone(),
                    script_id,
                    url.to_string(),
                ));
            }
        }
        "Debugger.paused" => {
            // 安全兜底：Debugger 域不设任何断点，但万一页面因其它原因暂停
            // （debugger 语句、外部探针），立即放行，绝不让渲染进程卡死。
            enqueue_debugger_resume(command_tx);
        }
        _ => {}
    }
}

fn bump_bootstrap_epoch(bootstrap_epoch_tx: &watch::Sender<u64>) {
    bootstrap_epoch_tx.send_modify(|epoch| *epoch += 1);
}

/// 当前主 frame 的 default 执行上下文。已知主 frame 时不回退到子 frame；
/// 尚无 frame 信息时暂取最新的 default 上下文，查询主 frame 后重新选择。
fn publish_main_context(
    contexts: &HashMap<u64, ContextInfo>,
    main_frame_id: &Arc<Mutex<Option<String>>>,
    main_context_tx: &watch::Sender<Option<u64>>,
    counters: &PatchSessionCounters,
    command_tx: &mpsc::UnboundedSender<SessionCommand>,
    republish_capabilities: bool,
) {
    let main_frame_id = main_frame_id.lock().expect("main frame id lock poisoned");
    let main = contexts
        .iter()
        .filter(|(_, info)| {
            info.is_default
                && main_frame_id
                    .as_deref()
                    .is_none_or(|frame| info.frame_id.as_deref() == Some(frame))
        })
        .map(|(id, _)| *id)
        .max();
    let changed = *main_context_tx.borrow() != main;
    if changed {
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "renderer_asset_patch.context_published",
            json!({
                "mainContext": main,
                "mainFrameId": main_frame_id.as_deref(),
                "contexts": contexts
                    .iter()
                    .map(|(id, info)| json!({
                        "id": id,
                        "frameId": info.frame_id,
                        "isDefault": info.is_default,
                    }))
                    .collect::<Vec<_>>(),
            }),
        );
        // 安装阶段可能还没有 receiver；send_replace 仍保留当前上下文。
        main_context_tx.send_replace(main);
    }
    if (changed || republish_capabilities)
        && counters.script_edit_disabled.load(Ordering::Relaxed) != 0
    {
        enqueue_script_edit_unavailable(command_tx, main);
    }
}

fn enqueue_script_edit_unavailable(
    command_tx: &mpsc::UnboundedSender<SessionCommand>,
    context_id: Option<u64>,
) {
    let Some(context_id) = context_id else {
        return;
    };
    let _ = command_tx.send(SessionCommand::CdpFireAndForget {
        method: "Runtime.evaluate",
        params: json!({
            "expression": "(()=>{window.__codeyRendererAssetPatchCapabilities={...window.__codeyRendererAssetPatchCapabilities,liveEdit:'unavailable'};})()",
            "contextId": context_id,
            "returnByValue": true,
        }),
    });
}

fn record_script_edit_unavailable(
    error: &anyhow::Error,
    counters: &PatchSessionCounters,
    command_tx: &mpsc::UnboundedSender<SessionCommand>,
    context_id: Option<u64>,
) {
    if format!("{error:#}").contains("setScriptSource functionality no longer available")
        && counters
            .script_edit_disabled
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        // 仅公布 Live Edit 的能力；Fetch 与运行时请求入口仍可继续工作。
        enqueue_script_edit_unavailable(command_tx, context_id);
    }
}

/// 入口文档判定：`app://` 协议的 index.html。允许带 query/fragment——首导航会带
/// 初始化参数，页面侧的同一判定按 `pathname` 比较，这里必须等价。
fn is_bootstrap_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    path.starts_with("app://") && path.to_ascii_lowercase().ends_with("/index.html")
}

/// Rust 侧的廉价候选过滤，只决定是否值得读响应体并跑 evaluate；真正的补丁
/// 适用性判断仍在页面里的补丁入口内完成，规则只有一份。
fn is_patch_candidate_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("app://") else {
        return false;
    };
    let path = rest.find('/').map(|index| &rest[index..]).unwrap_or("");
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    path.ends_with(".html")
        || (path.contains("/assets/")
            && (path.ends_with(".js") || path.ends_with(".cjs") || path.ends_with(".mjs")))
}

/// 放行一个暂停的请求：命令入队即返回，不需要等待 CDP 响应。放行失败也不影响
/// 会话，泵退出时待处理的暂停会以失败收尾。
fn enqueue_continue_request(command_tx: &mpsc::UnboundedSender<SessionCommand>, request_id: &str) {
    let _ = command_tx.send(SessionCommand::CdpFireAndForget {
        method: "Fetch.continueRequest",
        params: json!({ "requestId": request_id }),
    });
}

fn enqueue_debugger_resume(command_tx: &mpsc::UnboundedSender<SessionCommand>) {
    let _ = command_tx.send(SessionCommand::CdpFireAndForget {
        method: "Debugger.resume",
        params: json!({}),
    });
}

/// 与页面侧 isCodexRendererAssetUrl 静态名单保持一致，限制 scriptParsed
/// 通道的源码读取及诊断；具体补丁是否适用仍由页面入口决定。
fn is_patchable_named_asset(url: &str) -> bool {
    let base = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    [
        "app-initial",
        "app-primary",
        "app-shared",
        "codex-composer-adapter",
        "general-settings",
        "model-list-filter",
        "windows-model-controls",
        "use-service-tier-settings",
        "read-service-tier-for-request",
        "subagent-activity-chip-group",
        "appearance-settings",
        "pet-settings",
        "pets-settings",
    ]
    .iter()
    .any(|prefix| base.starts_with(prefix))
}

fn log_script_patch_stage(url: &str, stage: &str, detail: &str) {
    if !is_patchable_named_asset(url) {
        return;
    }
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "renderer_asset_patch.script_patch",
        json!({ "url": url, "stage": stage, "detail": detail }),
    );
}

/// scriptParsed 的异步备用改写，不暂停脚本求值，不能保证阻止静态 import
/// 及初始化副作用；运行时请求入口仍负责路由校验。
async fn handle_parsed_script(
    command_tx: mpsc::UnboundedSender<SessionCommand>,
    mut context_rx: watch::Receiver<Option<u64>>,
    install_script: Arc<str>,
    evaluator: Arc<PatchEvaluator>,
    counters: Arc<PatchSessionCounters>,
    script_id: String,
    url: String,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let failed = || counters.script_patch_failed.fetch_add(1, Relaxed);
    if counters.script_edit_disabled.load(Relaxed) != 0 {
        return;
    }
    let source_response = send_command_via(
        &command_tx,
        "Debugger.getScriptSource",
        json!({ "scriptId": script_id }),
    )
    .await;
    // 其他并发任务可能在读源期间确认禁用；避免复制大模块或继续跨页面求值。
    if counters.script_edit_disabled.load(Relaxed) != 0 {
        return;
    }
    let source = match source_response {
        Ok(value) => match value
            .get("result")
            .and_then(|result| result.get("scriptSource"))
            .and_then(Value::as_str)
        {
            Some(source) => source.to_string(),
            None => {
                failed();
                return;
            }
        },
        Err(error) => {
            log_script_patch_stage(&url, "getScriptSource", &format!("{error:#}"));
            failed();
            return;
        }
    };
    // 与 Fetch 路径同一策略：优先辅助页，失败退回被拦页自己的上下文。
    let patched = match evaluator.evaluate(&url, &source).await {
        Ok(patched) => patched,
        Err(_) if counters.script_edit_disabled.load(Relaxed) != 0 => return,
        Err(evaluator_error) => match evaluate_in_page_context(
            &command_tx,
            &mut context_rx,
            &install_script,
            &url,
            &source,
        )
        .await
        {
            Ok(patched) => patched,
            Err(error) => {
                log_script_patch_stage(
                    &url,
                    "evaluate",
                    &format!("evaluator: {evaluator_error:#}; page: {error:#}"),
                );
                failed();
                return;
            }
        },
    };
    if counters.script_edit_disabled.load(Relaxed) != 0 {
        return;
    }
    let Some(patched) = patched.filter(|patched| *patched != source) else {
        log_script_patch_stage(&url, "no_patch", "entry returned null/identical");
        return;
    };
    match send_command_via(
        &command_tx,
        "Debugger.setScriptSource",
        json!({ "scriptId": script_id, "scriptSource": patched }),
    )
    .await
    {
        Ok(_) => {
            log_script_patch_stage(&url, "patched", "setScriptSource ok");
            counters.script_patched.fetch_add(1, Relaxed);
        }
        Err(error) => {
            log_script_patch_stage(&url, "setScriptSource", &format!("{error:#}"));
            record_script_edit_unavailable(&error, &counters, &command_tx, *context_rx.borrow());
            failed();
        }
    }
}

async fn send_command_via(
    command_tx: &mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
) -> Result<Value> {
    let (respond, receiver) = oneshot::channel();
    command_tx
        .send(SessionCommand::Cdp {
            method,
            params,
            respond,
        })
        .map_err(|_| anyhow::anyhow!("补丁会话已退出"))?;
    match tokio::time::timeout(PAUSED_COMMAND_TIMEOUT, receiver).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => bail!("CDP command {method} failed: {error}"),
        Ok(Err(_)) => bail!("补丁会话已退出，CDP command {method} 无响应"),
        Err(_) => bail!("CDP command {method} timed out"),
    }
}

async fn process_paused_asset(
    command_tx: mpsc::UnboundedSender<SessionCommand>,
    mut context_rx: watch::Receiver<Option<u64>>,
    install_script: Arc<str>,
    evaluator: Arc<PatchEvaluator>,
    counters: Arc<PatchSessionCounters>,
    params: Value,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(request_id) = params
        .get("requestId")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let url = params
        .get("request")
        .and_then(|request| request.get("url"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let continue_request = |command_tx: &mpsc::UnboundedSender<SessionCommand>| {
        enqueue_continue_request(command_tx, &request_id);
    };

    let skip = |counters: &PatchSessionCounters| {
        counters.skipped.fetch_add(1, Relaxed);
        continue_request(&command_tx);
    };
    let body = match send_command_via(
        &command_tx,
        "Fetch.getResponseBody",
        json!({ "requestId": request_id }),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            log_patch_skip(&url, format!("getResponseBody: {error:#}"));
            skip(&counters);
            return;
        }
    };
    let raw_body = body
        .get("result")
        .and_then(|result| result.get("body"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let base64_encoded = body
        .get("result")
        .and_then(|result| result.get("base64Encoded"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let decoded = if base64_encoded {
        base64::engine::general_purpose::STANDARD
            .decode(raw_body)
            .unwrap_or_default()
    } else {
        raw_body.as_bytes().to_vec()
    };
    let Ok(source) = String::from_utf8(decoded) else {
        skip(&counters);
        return;
    };

    counters.evaluated.fetch_add(1, Relaxed);
    // 优先在辅助页的稳定上下文里求值；没有辅助 target 或求值失败时退回被
    // 拦页自己的上下文。
    let patched = match evaluator.evaluate(&url, &source).await {
        Ok(patched) => patched,
        Err(evaluator_error) => {
            match evaluate_in_page_context(
                &command_tx,
                &mut context_rx,
                &install_script,
                &url,
                &source,
            )
            .await
            {
                Ok(patched) => patched,
                Err(error) => {
                    log_patch_skip(
                        &url,
                        format!("patch evaluate: {error:#}; evaluator: {evaluator_error:#}"),
                    );
                    skip(&counters);
                    return;
                }
            }
        }
    };
    let Some(patched) = patched else {
        skip(&counters);
        return;
    };

    let response_code = params
        .get("responseStatusCode")
        .and_then(Value::as_i64)
        .unwrap_or(200) as i32;
    let headers = fresh_response_headers(
        params
            .get("responseHeaders")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    let fulfill = send_command_via(
        &command_tx,
        "Fetch.fulfillRequest",
        json!({
            "requestId": request_id,
            "responseCode": response_code,
            "responseHeaders": headers,
            "body": base64::engine::general_purpose::STANDARD.encode(patched.as_bytes()),
        }),
    )
    .await;
    if let Err(error) = fulfill {
        log_patch_skip(&url, format!("fulfillRequest: {error:#}"));
        skip(&counters);
    } else {
        counters.patched.fetch_add(1, Relaxed);
    }
}

/// 等一个可用的主执行上下文。`stale` 为 Some 时要求等到一个不同的 id——
/// 用于导航销毁旧上下文后的重试，期间通道短暂回到 None 也继续等。
async fn wait_for_context(
    context_rx: &mut watch::Receiver<Option<u64>>,
    stale: Option<u64>,
) -> Option<u64> {
    let usable = |current: Option<u64>| current.is_some() && current != stale;
    if usable(*context_rx.borrow()) {
        return *context_rx.borrow();
    }
    tokio::time::timeout(CONTEXT_WAIT_BUDGET, async {
        while !usable(*context_rx.borrow()) {
            if context_rx.changed().await.is_err() {
                return None;
            }
        }
        *context_rx.borrow()
    })
    .await
    .ok()
    .flatten()
}

/// 在被拦页自己的 default 上下文里跑补丁入口。辅助页求值不可用时的回退
/// 路径：导航窗口内该上下文会被 Runtime agent 暂时拒绝，所以带等上下文与
/// 短重试。
async fn evaluate_in_page_context(
    command_tx: &mpsc::UnboundedSender<SessionCommand>,
    context_rx: &mut watch::Receiver<Option<u64>>,
    install_script: &Arc<str>,
    url: &str,
    source: &str,
) -> Result<Option<String>> {
    // 等主 frame 的 default 执行上下文：没有它就跑不了改写 evaluate。
    let Some(mut context_id) = wait_for_context(context_rx, None).await else {
        bail!("waiting for renderer execution context");
    };
    // 入口不在当前文档时（会话建立晚于文档创建）就地安装：补丁脚本是幂等的
    // 纯函数 IIFE，在 CDP 编译的函数体内直接执行，不经过 eval，也不触发
    // 页面导航。文档脚本仍会负责后续新文档的预装。
    let function_declaration = format!(
        "(u,s)=>{{try{{if(typeof globalThis.__codeyRendererPatchEntry!==\"function\"){{{install_script}}};return typeof globalThis.__codeyRendererPatchEntry===\"function\"?globalThis.__codeyRendererPatchEntry(u,s):null}}catch(_){{return null}}}}",
        install_script = install_script
    );
    // 导航窗口内 callFunctionOn 可能拿到两类暂时性失败：暂停事件先于
    // executionContextsCleared 到达时快照到旧文档的上下文 id；新上下文刚由
    // executionContextCreated 发布时 Runtime agent 也可能短暂拒绝执行。
    // 每次失败都先短等通道发布新 id，等不到也用同一 id 再试——两种窗口都会
    // 自己消退。
    let mut last_error = None;
    for attempt in 0..4 {
        if attempt > 0 {
            context_id = settle_context(context_rx, context_id).await;
        }
        let call = send_command_via(
            command_tx,
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": function_declaration,
                "arguments": [
                    { "value": url },
                    { "value": source },
                ],
                "executionContextId": context_id,
                "returnByValue": true,
            }),
        )
        .await;
        match call {
            Ok(value) => {
                return Ok(value
                    .get("result")
                    .and_then(|result| result.get("result"))
                    .and_then(|result| result.get("value"))
                    .and_then(Value::as_str)
                    .map(str::to_string));
            }
            Err(error) => {
                if !format!("{error:#}").contains("Cannot find context") {
                    return Err(error);
                }
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("page context patch evaluate unavailable")))
}

/// 重试前的短窗口：优先等一个不同的上下文 id（旧 id 可能已被销毁），等不到
/// 就用当前值——新上下文刚创建时被短暂拒绝的场景靠下一次尝试覆盖。
const CONTEXT_SETTLE_BUDGET: Duration = Duration::from_millis(1500);

async fn settle_context(context_rx: &mut watch::Receiver<Option<u64>>, current: u64) -> u64 {
    let _ = tokio::time::timeout(CONTEXT_SETTLE_BUDGET, async {
        while *context_rx.borrow() == Some(current) || context_rx.borrow().is_none() {
            if context_rx.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    (*context_rx.borrow()).unwrap_or(current)
}

fn record_patch_log(payload: Option<&str>) {
    let Some(payload) = payload else { return };
    let parsed: Value = serde_json::from_str(payload).unwrap_or_else(|_| json!({}));
    let operation = parsed
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("renderer_asset_patch");
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or(payload);
    let context = parsed.get("context").cloned().unwrap_or_else(|| json!({}));
    error_log::record_failure("patch_failed", operation, message, context);
}

fn log_patch_skip(url: &str, reason: String) {
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "renderer_asset_patch.request_skipped",
        json!({ "url": url, "reason": reason }),
    );
}

/// 改写后的资产长度、摘要与编码都变了，协议头里对应的字段必须剥掉，否则
/// Chromium 会按旧的长度或编码解析新正文。无名头一并丢弃。
fn fresh_response_headers(headers: &[Value]) -> Vec<Value> {
    const STALE_RESPONSE_HEADERS: [&str; 6] = [
        "content-encoding",
        "content-length",
        "content-md5",
        "digest",
        "etag",
        "last-modified",
    ];
    headers
        .iter()
        .filter(|header| {
            header
                .get("name")
                .and_then(Value::as_str)
                .map(|name| !STALE_RESPONSE_HEADERS.contains(&name.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_evaluator(command_tx: &mpsc::UnboundedSender<SessionCommand>) -> Arc<PatchEvaluator> {
        Arc::new(PatchEvaluator {
            own_ws_url: String::new(),
            install_script: Arc::from(""),
            connection: tokio::sync::Mutex::new(Some(EvaluatorConn {
                command_tx: command_tx.clone(),
                pump: tokio::spawn(std::future::pending()),
            })),
        })
    }

    async fn next_cdp_command(
        commands: &mut mpsc::UnboundedReceiver<SessionCommand>,
        expected: &str,
    ) -> (Value, oneshot::Sender<CommandResult>) {
        let command = tokio::time::timeout(Duration::from_secs(1), commands.recv())
            .await
            .expect("CDP command should be queued")
            .expect("CDP command channel should remain open");
        let SessionCommand::Cdp {
            method,
            params,
            respond,
        } = command
        else {
            panic!("expected a CDP command with a response");
        };
        assert_eq!(method, expected);
        (params, respond)
    }

    fn assert_capability_notification(
        commands: &mut mpsc::UnboundedReceiver<SessionCommand>,
        context_id: u64,
    ) {
        let SessionCommand::CdpFireAndForget { method, params } = commands
            .try_recv()
            .expect("capability notification should be queued")
        else {
            panic!("capability notification must not wait for a response");
        };
        assert_eq!(method, "Runtime.evaluate");
        assert_eq!(params["contextId"], context_id);
        let expression = params["expression"].as_str().unwrap();
        assert!(expression.contains("window.__codeyRendererAssetPatchCapabilities"));
        assert!(expression.contains("...window.__codeyRendererAssetPatchCapabilities"));
        assert!(expression.contains("liveEdit:'unavailable'"));
    }

    async fn prepare_script_write(
        command_tx: &mpsc::UnboundedSender<SessionCommand>,
        commands: &mut mpsc::UnboundedReceiver<SessionCommand>,
        context_rx: watch::Receiver<Option<u64>>,
        counters: Arc<PatchSessionCounters>,
    ) -> (tokio::task::JoinHandle<()>, oneshot::Sender<CommandResult>) {
        let task = tokio::spawn(handle_parsed_script(
            command_tx.clone(),
            context_rx,
            Arc::from(""),
            mock_evaluator(command_tx),
            counters,
            "script".to_string(),
            "app://-/assets/app-primary-test.js".to_string(),
        ));
        let (_, respond) = next_cdp_command(commands, "Debugger.getScriptSource").await;
        respond
            .send(Ok(json!({"result": {"scriptSource": "original()"}})))
            .unwrap();
        let (_, respond) = next_cdp_command(commands, "Runtime.evaluate").await;
        respond
            .send(Ok(json!({"result": {"result": {"value": "patched()"}}})))
            .unwrap();
        let (_, respond) = next_cdp_command(commands, "Debugger.setScriptSource").await;
        (task, respond)
    }

    #[tokio::test]
    async fn script_edit_requires_explicit_unavailability_and_notifies_only_once() {
        let (command_tx, mut commands) = mpsc::unbounded_channel();
        let (main_context, context_rx) = watch::channel(Some(42));
        let counters = Arc::new(PatchSessionCounters::default());
        let session = RendererAssetPatchSession {
            websocket_url: Arc::from("ws://test"),
            command_tx: command_tx.clone(),
            bootstrap_epoch: watch::channel(0).0,
            main_context,
            counters: counters.clone(),
            pump: Mutex::new(None),
        };
        let initial_stats = serde_json::to_value(session.stats()).unwrap();
        assert_eq!(initial_stats["scriptEditDisabled"], false);
        assert_eq!(initial_stats["pausedRequests"], 0);
        assert_eq!(initial_stats["scriptParsed"], 0);
        for error in [
            "Cannot find script",
            "CDP command Debugger.setScriptSource timed out",
            "another functionality no longer available",
        ] {
            let (task, respond) = prepare_script_write(
                &command_tx,
                &mut commands,
                context_rx.clone(),
                counters.clone(),
            )
            .await;
            respond.send(Err(error.to_string())).unwrap();
            task.await.unwrap();
            assert_eq!(counters.script_edit_disabled.load(Ordering::Relaxed), 0);
            assert!(commands.try_recv().is_err());
        }

        // 两个并发脚本都已发出写入，再先后收到同一个明确禁用错误。
        let (first, first_response) = prepare_script_write(
            &command_tx,
            &mut commands,
            context_rx.clone(),
            counters.clone(),
        )
        .await;
        let (second, second_response) =
            prepare_script_write(&command_tx, &mut commands, context_rx, counters.clone()).await;
        let unavailable = json!({"error": {"code": -32000, "message":
            "setScriptSource functionality no longer available"}})
        .to_string();
        first_response.send(Err(unavailable.clone())).unwrap();
        first.await.unwrap();
        assert_capability_notification(&mut commands, 42);
        second_response.send(Err(unavailable)).unwrap();
        second.await.unwrap();
        assert!(commands.try_recv().is_err());
        assert_eq!(counters.script_patch_failed.load(Ordering::Relaxed), 5);

        let stats = serde_json::to_value(session.stats()).unwrap();
        assert_eq!(stats["scriptEditDisabled"], true);
        assert_eq!(stats["pausedRequests"], 0);
    }

    #[tokio::test]
    async fn script_edit_capability_follows_main_default_context_across_navigation() {
        let (command_tx, mut commands) = mpsc::unbounded_channel();
        let (main_context_tx, context_rx) = watch::channel(None);
        drop(context_rx); // 初始化阶段没有订阅者，也必须保留选中的主上下文。
        let main_frame_id = Arc::new(Mutex::new(Some("main".to_string())));
        let counters = Arc::new(PatchSessionCounters::default());
        let bootstrap_epoch_tx = watch::channel(0).0;
        let install_script = Arc::from("");
        let evaluator = mock_evaluator(&command_tx);
        let mut contexts = HashMap::new();
        let mut inflight = tokio::task::JoinSet::new();
        let mut state = PatchEventState {
            contexts: &mut contexts,
            main_frame_id: &main_frame_id,
            main_context_tx: &main_context_tx,
            bootstrap_epoch_tx: &bootstrap_epoch_tx,
            command_tx: &command_tx,
            install_script: &install_script,
            evaluator: &evaluator,
            counters: &counters,
            inflight: &mut inflight,
        };
        record_script_edit_unavailable(
            &anyhow::anyhow!("setScriptSource functionality no longer available"),
            &counters,
            &command_tx,
            None,
        );
        let context = |id, frame, is_default| {
            json!({
                "method": "Runtime.executionContextCreated",
                "params": {"context": {"id": id, "auxData": {
                    "frameId": frame, "isDefault": is_default,
                }}},
            })
        };
        handle_patch_event(&context(90, "child", true), &mut state);
        handle_patch_event(&context(91, "main", false), &mut state);
        assert_eq!(*main_context_tx.borrow(), None);
        assert!(commands.try_recv().is_err());
        handle_patch_event(&context(2, "main", true), &mut state);
        assert_eq!(*main_context_tx.borrow(), Some(2));
        assert_capability_notification(&mut commands, 2);
        handle_patch_event(&context(92, "main", false), &mut state);
        assert!(commands.try_recv().is_err());

        handle_patch_event(
            &json!({"method": "Page.frameNavigated",
            "params": {"frame": {"id": "next-main"}}}),
            &mut state,
        );
        assert_eq!(*main_context_tx.borrow(), None);
        assert!(commands.try_recv().is_err());
        handle_patch_event(&context(3, "next-main", true), &mut state);
        assert_capability_notification(&mut commands, 3);
        handle_patch_event(
            &json!({"method": "Page.frameNavigated",
            "params": {"frame": {"id": "next-main"}}}),
            &mut state,
        );
        assert_capability_notification(&mut commands, 3);
        handle_patch_event(
            &json!({"method": "Runtime.executionContextDestroyed",
            "params": {"executionContextId": 3}}),
            &mut state,
        );
        assert_eq!(*main_context_tx.borrow(), None);
        assert!(commands.try_recv().is_err());
        handle_patch_event(
            &json!({"method": "Runtime.executionContextsCleared"}),
            &mut state,
        );
        handle_patch_event(&context(4, "next-main", true), &mut state);
        assert_capability_notification(&mut commands, 4);
    }

    #[tokio::test]
    async fn unavailable_script_edit_stops_pending_source_work_but_keeps_fetch_patching() {
        let (command_tx, mut commands) = mpsc::unbounded_channel();
        let (_main_context, context_rx) = watch::channel(Some(42));
        let counters = Arc::new(PatchSessionCounters::default());
        let (first, respond) = prepare_script_write(
            &command_tx,
            &mut commands,
            context_rx.clone(),
            counters.clone(),
        )
        .await;
        let pending = tokio::spawn(handle_parsed_script(
            command_tx.clone(),
            context_rx.clone(),
            Arc::from(""),
            mock_evaluator(&command_tx),
            counters.clone(),
            "pending".to_string(),
            "app://-/assets/app-shared-test.js".to_string(),
        ));
        let (_, source_response) =
            next_cdp_command(&mut commands, "Debugger.getScriptSource").await;
        respond
            .send(Err(
                "setScriptSource functionality no longer available".to_string()
            ))
            .unwrap();
        first.await.unwrap();
        assert_capability_notification(&mut commands, 42);
        source_response
            .send(Ok(json!({"result": {"scriptSource": "original()"}})))
            .unwrap();
        pending.await.unwrap();
        assert!(commands.try_recv().is_err());
        assert_eq!(counters.script_patch_failed.load(Ordering::Relaxed), 1);

        handle_parsed_script(
            command_tx.clone(),
            context_rx.clone(),
            Arc::from(""),
            mock_evaluator(&command_tx),
            counters.clone(),
            "later".to_string(),
            "app://-/assets/app-shared-test.js".to_string(),
        )
        .await;
        assert!(commands.try_recv().is_err());

        let fetch = tokio::spawn(process_paused_asset(
            command_tx.clone(),
            context_rx,
            Arc::from(""),
            mock_evaluator(&command_tx),
            counters.clone(),
            json!({"requestId": "fetch", "request": {
                "url": "app://-/assets/app-primary-test.js",
            }}),
        ));
        let (_, respond) = next_cdp_command(&mut commands, "Fetch.getResponseBody").await;
        respond
            .send(Ok(json!({"result": {"body": "original()"}})))
            .unwrap();
        let (_, respond) = next_cdp_command(&mut commands, "Runtime.evaluate").await;
        respond
            .send(Ok(json!({"result": {"result": {"value": "patched()"}}})))
            .unwrap();
        let (params, respond) = next_cdp_command(&mut commands, "Fetch.fulfillRequest").await;
        assert_eq!(params["requestId"], "fetch");
        assert_eq!(
            params["body"],
            base64::engine::general_purpose::STANDARD.encode("patched()")
        );
        respond.send(Ok(json!({}))).unwrap();
        fetch.await.unwrap();
        assert_eq!(counters.patched.load(Ordering::Relaxed), 1);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn patch_candidates_cover_entry_documents_and_bundled_assets() {
        for accepted in [
            "app://-/index.html",
            "app://-/index.html?initialRoute=%2Fsettings",
            "app://-/assets/app-initial-BHB6SClA.js",
            "app://-/assets/index-BZNttYfb.js?build=store",
            "app://-/assets/chunk.cjs",
        ] {
            assert!(is_patch_candidate_url(accepted), "{accepted}");
        }
        // 其他 scheme、非 assets 目录、非脚本扩展名都不值得读响应体。
        for rejected in [
            "https://example.com/assets/app-initial.js",
            "app://-/fonts/inter.woff2",
            "app://-/assets/app-initial.css",
            "app://-/assets/app-initial.js.map",
        ] {
            assert!(!is_patch_candidate_url(rejected), "{rejected}");
        }
    }

    #[test]
    fn primary_composer_asset_is_available_to_script_patching() {
        let url = "app://-/assets/app-primary-b4ce9e46e8cf.js?build=desktop";
        assert!(is_patch_candidate_url(url));
        assert!(is_patchable_named_asset(url));
        assert!(!is_patchable_named_asset(
            "app://-/assets/chatgpt-composer.js"
        ));
    }

    #[test]
    fn bootstrap_url_tolerates_initial_navigation_parameters() {
        for accepted in [
            "app://-/index.html",
            "app://-/INDEX.HTML?x=1",
            "app://-/index.html#/settings",
        ] {
            assert!(is_bootstrap_url(accepted), "{accepted}");
        }
        for rejected in [
            "app://-/assets/index.html.js",
            "app://-/index.htm",
            "https://example.com/index.html",
        ] {
            assert!(!is_bootstrap_url(rejected), "{rejected}");
        }
    }

    #[test]
    fn debugger_port_is_read_from_the_websocket_url() {
        assert_eq!(
            ws_url_port("ws://127.0.0.1:9222/devtools/page/ABC"),
            Some(9222)
        );
        assert_eq!(
            ws_url_port("ws://localhost:1234/devtools/browser/x"),
            Some(1234)
        );
        assert_eq!(ws_url_port("not-a-websocket-url"), None);
        assert_eq!(ws_url_port("ws://127.0.0.1/devtools/page/ABC"), None);
    }

    #[test]
    fn rewritten_assets_drop_stale_framing_headers() {
        let headers = json!([
            { "name": "Content-Type", "value": "text/javascript" },
            { "name": "content-length", "value": "1234" },
            { "name": "ETag", "value": "\"abc\"" },
            { "name": "Content-Encoding", "value": "br" },
            { "value": "no-name-is-dropped" },
        ]);
        let kept = fresh_response_headers(headers.as_array().expect("头是数组"));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["name"], "Content-Type");
    }
}
