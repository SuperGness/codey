//! 渲染资产补丁服务：一条浏览器级 CDP 连接覆盖 Codex 的全部页面。
//!
//! Electron fuse 关闭后主进程 `protocol.handle` 改写不可用，这里改用 Fetch 域
//! 改写 `app://` 脚本响应。新页面经自动挂载在创建时暂停，Fetch 拦截落位后才
//! 放行；连接前已存在的页面直接挂上，只要尚未导航就能覆盖首个文档。拦截只匹配
//! 补丁名单里的脚本，其余资产不经过 Codey。
//!
//! 补丁入口是纯函数，结果只取决于补丁脚本与资产内容，而 Codex 的资产文件名
//! 自带内容哈希：结果按「补丁脚本版本 + 文件名」缓存到磁盘，命中时在请求阶段
//! 直接 fulfill；未命中才取响应正文，在请求它的页面里跑一次补丁入口。错过首个
//! 文档的页面不强制重载，只在诊断里记为未覆盖。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::error_log;

/// 补丁脚本注入页面后暴露的纯函数入口：`(url, source) => patched | null`。
const RENDERER_ASSET_PATCH_SCRIPT: &str = include_str!("renderer_asset_patch.js");
/// 需要改写的脚本名前缀，与页面侧 isCodexRendererAssetUrl 的名单保持一致。
const PATCHABLE_ASSET_PREFIXES: [&str; 3] = ["app-initial", "app-primary", "app-shared"];
/// 仅在关闭宠物时改写的设置页脚本。
const PET_ASSET_PREFIXES: [&str; 1] = ["pets-settings"];
/// 等 Codex 调试端点就绪的总预算；超时后由下一次注入重新拉起服务。
const CONNECT_BUDGET: Duration = Duration::from_secs(60);
/// 端点未就绪时的重连间隔：主窗口通常在端点出现后数百毫秒内导航，间隔要短。
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
/// 单条命令的应答预算；最慢的是在页面里改写十几 MB 的 bundle。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
/// 整份 bundle 经 base64 后接近 20 MB，Chromium 把每条消息作为单帧发送。
const MAX_MESSAGE_BYTES: usize = 256 << 20;
/// 同时在页面里改写的资产数：每份改写都要在内存里持有数倍于 bundle 的副本。
const MAX_CONCURRENT_PATCHES: usize = 2;
/// 磁盘缓存上限，约可容纳三个 Codex 版本的补丁结果。
const MAX_CACHE_BYTES: u64 = 96 << 20;
const CACHE_DIR_NAME: &str = "renderer-patch-cache";

static NEXT_MESSAGE_ID: AtomicU64 = AtomicU64::new(1);
static SERVICE: OnceLock<Mutex<Option<ServiceHandle>>> = OnceLock::new();

fn next_message_id() -> u64 {
    NEXT_MESSAGE_ID.fetch_add(1, Ordering::Relaxed)
}

fn service_slot() -> &'static Mutex<Option<ServiceHandle>> {
    SERVICE.get_or_init(|| Mutex::new(None))
}

/// 补丁脚本注入页面用的完整脚本：先放选项全局，再定义入口。
pub(crate) fn renderer_asset_patch_document_script(disable_pet: bool) -> String {
    let options = json!({ "disablePet": disable_pet });
    format!(
        "globalThis.__CODEY_RENDERER_PATCH_OPTIONS__ = {options};\n{RENDERER_ASSET_PATCH_SCRIPT}"
    )
}

struct ServiceHandle {
    debug_port: u16,
    disable_pet: bool,
    context: Arc<ServiceContext>,
    task: tokio::task::JoinHandle<()>,
}

/// Codex 启动前调用：替换旧服务，从端点一出现就开始挂载页面。
pub(crate) fn start_service(debug_port: u16, disable_pet: bool) {
    let context = Arc::new(ServiceContext::new(disable_pet, default_cache_dir()));
    let task = tokio::spawn(run_service(debug_port, context.clone()));
    let previous = service_slot()
        .lock()
        .expect("patch service slot poisoned")
        .replace(ServiceHandle {
            debug_port,
            disable_pet,
            context,
            task,
        });
    if let Some(previous) = previous {
        previous.task.abort();
    }
}

/// 注入路径调用：同一端口的服务仍在运行时什么都不做。
pub(crate) fn ensure_service(debug_port: u16, disable_pet: bool) {
    let running = service_slot()
        .lock()
        .expect("patch service slot poisoned")
        .as_ref()
        .is_some_and(|service| {
            service.debug_port == debug_port
                && service.disable_pet == disable_pet
                && !service.task.is_finished()
        });
    if !running {
        start_service(debug_port, disable_pet);
    }
}

/// 某个页面的拦截统计；服务未挂上该页面时返回 None。
pub(crate) fn page_stats(target_id: &str) -> Option<PageStats> {
    let slot = service_slot().lock().expect("patch service slot poisoned");
    slot.as_ref()?.context.page_stats(target_id)
}

/// `attachedBeforeLoad` 为 false 且 `patched` 为 0 说明挂上时首个文档已加载，
/// 本次运行的主 bundle 没有补丁。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageStats {
    pub(crate) attached_before_load: bool,
    pub(crate) intercepted: u64,
    pub(crate) patched: u64,
    pub(crate) cache_hits: u64,
    pub(crate) skipped: u64,
}

struct PageEntry {
    target_id: String,
    stats: PageStats,
}

struct ServiceContext {
    install_script: Arc<str>,
    patterns: Value,
    cache: PatchCache,
    /// 按扁平化会话 id 记录已挂上的页面。
    pages: Mutex<HashMap<String, PageEntry>>,
    permits: tokio::sync::Semaphore,
}

impl ServiceContext {
    fn new(disable_pet: bool, cache_dir: PathBuf) -> Self {
        let install_script: Arc<str> = Arc::from(renderer_asset_patch_document_script(disable_pet));
        Self {
            cache: PatchCache::new(cache_dir, &install_script),
            install_script,
            patterns: fetch_patterns(disable_pet),
            pages: Mutex::new(HashMap::new()),
            permits: tokio::sync::Semaphore::new(MAX_CONCURRENT_PATCHES),
        }
    }

    fn page_stats(&self, target_id: &str) -> Option<PageStats> {
        self.pages
            .lock()
            .expect("patch page registry poisoned")
            .values()
            .find(|page| page.target_id == target_id)
            .map(|page| page.stats)
    }

    fn update_page(&self, session_id: &str, update: impl FnOnce(&mut PageStats)) {
        if let Some(page) = self
            .pages
            .lock()
            .expect("patch page registry poisoned")
            .get_mut(session_id)
        {
            update(&mut page.stats);
        }
    }
}

fn default_cache_dir() -> PathBuf {
    codey_runtime_core::paths::default_app_state_dir().join(CACHE_DIR_NAME)
}

/// 只在请求阶段拦截名单内的脚本；未命中缓存的请求再要求暂停在响应阶段。
fn fetch_patterns(disable_pet: bool) -> Value {
    let pet: &[&str] = if disable_pet {
        &PET_ASSET_PREFIXES
    } else {
        &[]
    };
    PATCHABLE_ASSET_PREFIXES
        .iter()
        .chain(pet)
        .map(|prefix| {
            json!({
                "urlPattern": format!("app://*/assets/{prefix}*"),
                "resourceType": "Script",
                "requestStage": "Request",
            })
        })
        .collect()
}

type CommandResult = Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<CommandResult>>>>;

/// 浏览器端点上的命令通道；会话级命令带扁平化 sessionId。
#[derive(Clone)]
struct BrowserConnection {
    outgoing: mpsc::UnboundedSender<Message>,
    pending: Pending,
}

impl BrowserConnection {
    fn frame(id: u64, method: &str, params: Value, session_id: Option<&str>) -> Message {
        let mut payload = json!({ "id": id, "method": method, "params": params });
        if let Some(session_id) = session_id {
            payload["sessionId"] = json!(session_id);
        }
        Message::Text(payload.to_string().into())
    }

    async fn call(&self, method: &str, params: Value, session_id: Option<&str>) -> Result<Value> {
        let id = next_message_id();
        let (respond, response) = oneshot::channel();
        self.pending
            .lock()
            .expect("patch command registry poisoned")
            .insert(id, respond);
        if self
            .outgoing
            .send(Self::frame(id, method, params, session_id))
            .is_err()
        {
            self.pending
                .lock()
                .expect("patch command registry poisoned")
                .remove(&id);
            bail!("补丁服务连接已关闭，CDP command {method} 未发送");
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, response).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => bail!("CDP command {method} failed: {error}"),
            Ok(Err(_)) => bail!("补丁服务连接已关闭，CDP command {method} 无响应"),
            Err(_) => {
                self.pending
                    .lock()
                    .expect("patch command registry poisoned")
                    .remove(&id);
                bail!("CDP command {method} timed out")
            }
        }
    }

    /// 不需要结果的命令（放行、恢复、分离）：应答到达时没有登记，直接丢弃。
    fn notify(&self, method: &str, params: Value, session_id: Option<&str>) {
        let _ = self
            .outgoing
            .send(Self::frame(next_message_id(), method, params, session_id));
    }
}

async fn run_service(debug_port: u16, context: Arc<ServiceContext>) {
    let socket = match connect_browser(debug_port).await {
        Ok(socket) => socket,
        Err(error) => {
            log_service_event(
                "renderer_asset_patch.service_unavailable",
                json!({ "message": format!("{error:#}") }),
            );
            return;
        }
    };
    let prune_cache = context.cache.clone();
    tokio::task::spawn_blocking(move || prune_cache.prune());
    if let Err(error) = serve(socket, context).await {
        log_service_event(
            "renderer_asset_patch.service_failed",
            json!({ "message": format!("{error:#}") }),
        );
    }
}

async fn connect_browser(
    debug_port: u16,
) -> Result<codey_runtime_core::bridge::ConnectedCdpSocket> {
    let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
    loop {
        let attempt = async {
            let url = codey_runtime_core::cdp::browser_websocket_url(debug_port).await?;
            codey_runtime_core::bridge::connect_cdp_websocket_for_bulk_transfer(
                &url,
                MAX_MESSAGE_BYTES,
            )
            .await
        };
        match attempt.await {
            Ok(socket) => return Ok(socket),
            Err(error) if tokio::time::Instant::now() >= deadline => {
                return Err(error).context("等待 Codex 调试端点超时");
            }
            Err(_) => tokio::time::sleep(CONNECT_RETRY_INTERVAL).await,
        }
    }
}

/// 在已连上的浏览器端点上运行服务，直到连接关闭（Codex 退出）。连接断开时
/// Chromium 会分离全部会话，暂停中的页面与请求随之放行。
async fn serve<S>(socket: S, context: Arc<ServiceContext>) -> Result<()>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Send
        + 'static,
{
    let (mut writer, mut reader) = socket.split();
    let (outgoing, mut outgoing_rx) = mpsc::unbounded_channel::<Message>();
    let writer_task = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            if writer.send(message).await.is_err() {
                break;
            }
        }
        let _ = writer.close().await;
    });
    let connection = BrowserConnection {
        outgoing,
        pending: Pending::default(),
    };
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(attach_pages(connection.clone(), context.clone()));
    let result = loop {
        tokio::select! {
            message = reader.next() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(_))) | None => break Ok(()),
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => break Err(error).context("读取补丁服务 CDP 消息失败"),
                };
                let Ok(payload) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = payload.get("id").and_then(Value::as_u64) {
                    let respond = connection
                        .pending
                        .lock()
                        .expect("patch command registry poisoned")
                        .remove(&id);
                    if let Some(respond) = respond {
                        let _ = respond.send(match payload.get("error") {
                            Some(error) => Err(error.to_string()),
                            None => Ok(payload),
                        });
                    }
                    continue;
                }
                dispatch_event(payload, &connection, &context, &mut tasks);
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };
    connection
        .pending
        .lock()
        .expect("patch command registry poisoned")
        .drain()
        .for_each(|(_, respond)| {
            let _ = respond.send(Err("patch service closed".to_string()));
        });
    tasks.shutdown().await;
    drop(connection);
    let _ = writer_task.await;
    result
}

/// 开启自动挂载并挂上已存在的页面。自动挂载只覆盖之后创建的目标，主窗口常在
/// 端点就绪前就已创建、稍后才导航，必须显式挂上。
async fn attach_pages(connection: BrowserConnection, context: Arc<ServiceContext>) {
    if let Err(error) = connection
        .call(
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            None,
        )
        .await
    {
        log_service_event(
            "renderer_asset_patch.auto_attach_failed",
            json!({ "message": format!("{error:#}") }),
        );
    }
    let Ok(targets) = connection.call("Target.getTargets", json!({}), None).await else {
        return;
    };
    let target_ids: Vec<String> = targets["result"]["targetInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|target| target["type"] == "page")
        .filter_map(|target| target["targetId"].as_str().map(str::to_string))
        .collect();
    for target_id in target_ids {
        if context.page_stats(&target_id).is_none() {
            // 挂载结果经 Target.attachedToTarget 事件统一处理。
            let _ = connection
                .call(
                    "Target.attachToTarget",
                    json!({ "targetId": target_id, "flatten": true }),
                    None,
                )
                .await;
        }
    }
}

fn dispatch_event(
    payload: Value,
    connection: &BrowserConnection,
    context: &Arc<ServiceContext>,
    tasks: &mut tokio::task::JoinSet<()>,
) {
    let Some(method) = payload.get("method").and_then(Value::as_str) else {
        return;
    };
    match method {
        "Target.attachedToTarget" => {
            tasks.spawn(on_attached(
                connection.clone(),
                context.clone(),
                payload["params"].clone(),
            ));
        }
        "Target.detachedFromTarget" => {
            if let Some(session_id) = payload["params"]["sessionId"].as_str() {
                context
                    .pages
                    .lock()
                    .expect("patch page registry poisoned")
                    .remove(session_id);
            }
        }
        "Fetch.requestPaused" => {
            let Some(session_id) = payload["sessionId"].as_str().map(str::to_string) else {
                return;
            };
            tasks.spawn(on_request_paused(
                connection.clone(),
                context.clone(),
                session_id,
                payload["params"].clone(),
            ));
        }
        _ => {}
    }
}

async fn on_attached(connection: BrowserConnection, context: Arc<ServiceContext>, params: Value) {
    let Some(session_id) = params["sessionId"].as_str() else {
        return;
    };
    let target = &params["targetInfo"];
    let target_id = target["targetId"].as_str().unwrap_or_default();
    let url = target["url"].as_str().unwrap_or_default();
    let waiting = params["waitingForDebugger"].as_bool().unwrap_or(false);
    let resume = || {
        if waiting {
            connection.notify(
                "Runtime.runIfWaitingForDebugger",
                json!({}),
                Some(session_id),
            );
        }
    };
    let detach = || {
        connection.notify(
            "Target.detachFromTarget",
            json!({ "sessionId": session_id }),
            None,
        );
    };
    if target["type"] != "page" {
        resume();
        detach();
        return;
    }
    let attached_before_load = waiting || url.is_empty() || url == "about:blank";
    {
        let mut pages = context.pages.lock().expect("patch page registry poisoned");
        if pages.values().any(|page| page.target_id == target_id) {
            drop(pages);
            resume();
            detach();
            return;
        }
        pages.insert(
            session_id.to_string(),
            PageEntry {
                target_id: target_id.to_string(),
                stats: PageStats {
                    attached_before_load,
                    ..PageStats::default()
                },
            },
        );
    }
    // Fetch 拦截由浏览器侧处理，页面暂停时也会应答；落位后才放行页面。
    if let Err(error) = connection
        .call(
            "Fetch.enable",
            json!({ "patterns": context.patterns }),
            Some(session_id),
        )
        .await
    {
        log_service_event(
            "renderer_asset_patch.fetch_enable_failed",
            json!({ "targetId": target_id, "message": format!("{error:#}") }),
        );
    }
    resume();
    if !attached_before_load && url.starts_with("app://") {
        log_service_event(
            "renderer_asset_patch.late_attach",
            json!({ "targetId": target_id, "url": url.split(['?', '#']).next() }),
        );
    }
}

async fn on_request_paused(
    connection: BrowserConnection,
    context: Arc<ServiceContext>,
    session_id: String,
    params: Value,
) {
    let Some(request_id) = params["requestId"].as_str() else {
        return;
    };
    let url = params["request"]["url"].as_str().unwrap_or_default();
    let session = Some(session_id.as_str());
    let pass_through = || {
        connection.notify(
            "Fetch.continueRequest",
            json!({ "requestId": request_id }),
            session,
        );
    };
    let response_stage =
        params.get("responseStatusCode").is_some() || params.get("responseErrorReason").is_some();
    if !response_stage {
        context.update_page(&session_id, |stats| stats.intercepted += 1);
        match context.cache.load(url).await {
            Some(CachedAsset::Patched(asset)) => {
                let fulfilled = connection
                    .call(
                        "Fetch.fulfillRequest",
                        json!({
                            "requestId": request_id,
                            "responseCode": asset.status,
                            "responseHeaders": asset.headers,
                            "body": asset.body_base64,
                        }),
                        session,
                    )
                    .await;
                match fulfilled {
                    Ok(_) => context.update_page(&session_id, |stats| {
                        stats.patched += 1;
                        stats.cache_hits += 1;
                    }),
                    Err(error) => {
                        log_patch_skip(url, format!("fulfill cached: {error:#}"));
                        context.update_page(&session_id, |stats| stats.skipped += 1);
                        pass_through();
                    }
                }
            }
            Some(CachedAsset::Unchanged) => {
                context.update_page(&session_id, |stats| stats.cache_hits += 1);
                pass_through();
            }
            None => {
                let intercept = connection
                    .call(
                        "Fetch.continueRequest",
                        json!({ "requestId": request_id, "interceptResponse": true }),
                        session,
                    )
                    .await;
                if let Err(error) = intercept {
                    log_patch_skip(url, format!("interceptResponse: {error:#}"));
                    context.update_page(&session_id, |stats| stats.skipped += 1);
                    pass_through();
                }
            }
        }
        return;
    }
    let status = params["responseStatusCode"].as_i64().unwrap_or(0);
    if params.get("responseErrorReason").is_some() || !(200..300).contains(&status) {
        pass_through();
        return;
    }
    let Ok(_permit) = context.permits.acquire().await else {
        pass_through();
        return;
    };
    let headers = fresh_response_headers(
        params["responseHeaders"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    match patch_response(&connection, &context, session, request_id, url).await {
        Ok(Some(patched)) => {
            let body_base64 = base64::engine::general_purpose::STANDARD.encode(patched.as_bytes());
            let fulfilled = connection
                .call(
                    "Fetch.fulfillRequest",
                    json!({
                        "requestId": request_id,
                        "responseCode": status,
                        "responseHeaders": headers,
                        "body": body_base64,
                    }),
                    session,
                )
                .await;
            if let Err(error) = fulfilled {
                log_patch_skip(url, format!("fulfillRequest: {error:#}"));
                context.update_page(&session_id, |stats| stats.skipped += 1);
                pass_through();
                return;
            }
            context.update_page(&session_id, |stats| stats.patched += 1);
            let patch = StoredPatch {
                status,
                headers,
                body: patched,
            };
            context.cache.store(url, Some(patch)).await;
        }
        Ok(None) => {
            pass_through();
            context.cache.store(url, None).await;
        }
        Err(error) => {
            log_patch_skip(url, format!("{error:#}"));
            context.update_page(&session_id, |stats| stats.skipped += 1);
            pass_through();
        }
    }
}

/// 读取暂停响应的正文并在请求它的页面里跑补丁入口。`Ok(None)` 表示补丁确认
/// 不需要改写；页面里没装上入口或求值失败返回错误，结果不进缓存。
async fn patch_response(
    connection: &BrowserConnection,
    context: &ServiceContext,
    session: Option<&str>,
    request_id: &str,
    url: &str,
) -> Result<Option<String>> {
    let body = connection
        .call(
            "Fetch.getResponseBody",
            json!({ "requestId": request_id }),
            session,
        )
        .await?;
    let raw = body["result"]["body"].as_str().unwrap_or_default();
    let source = if body["result"]["base64Encoded"].as_bool().unwrap_or(false) {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(raw)
                .context("响应正文 base64 解码失败")?,
        )
        .context("响应正文不是 UTF-8")?
    } else {
        raw.to_string()
    };
    let response = connection
        .call(
            "Runtime.evaluate",
            json!({
                "expression": patch_expression(&context.install_script, url, &source)?,
                "returnByValue": true,
            }),
            session,
        )
        .await?;
    if let Some(exception) = response["result"].get("exceptionDetails") {
        bail!("补丁求值抛出异常：{exception}");
    }
    let outcome: PatchOutcome =
        serde_json::from_value(response["result"]["result"]["value"].clone())
            .context("补丁求值结果格式无效")?;
    for record in &outcome.logs {
        record_patch_log(record);
    }
    if !outcome.installed {
        bail!("页面未能装上补丁入口");
    }
    Ok(outcome.patched)
}

#[derive(Deserialize)]
struct PatchOutcome {
    installed: bool,
    #[serde(default)]
    patched: Option<String>,
    #[serde(default)]
    logs: Vec<String>,
}

/// 页面里的补丁入口缺失时就地安装（幂等）；失败记录经返回值带回，不依赖
/// CDP binding。
fn patch_expression(install_script: &str, url: &str, source: &str) -> Result<String> {
    Ok(format!(
        "(()=>{{const logs=[];const previous=globalThis.__codeyPatchLog;\
globalThis.__codeyPatchLog=(record)=>{{logs.push(String(record))}};\
try{{if(typeof globalThis.__codeyRendererPatchEntry!==\"function\"){{{install_script}\n}}\
const entry=globalThis.__codeyRendererPatchEntry;\
if(typeof entry!==\"function\")return{{installed:false,logs}};\
return{{installed:true,patched:entry({url},{source}),logs}}}}\
catch(error){{logs.push(String(error));return{{installed:false,logs}}}}\
finally{{if(previous===undefined)delete globalThis.__codeyPatchLog;else globalThis.__codeyPatchLog=previous}}}})()",
        url = serde_json::to_string(url).context("补丁 URL 序列化失败")?,
        source = serde_json::to_string(source).context("补丁源码序列化失败")?,
    ))
}

/// 补丁结果磁盘缓存。键为补丁脚本摘要加资产文件名，只缓存文件名带内容哈希的
/// 资产；正文先于元数据落盘，元数据缺失或长度不符一律按未命中处理。
#[derive(Clone)]
struct PatchCache {
    dir: PathBuf,
    tag: String,
}

enum CachedAsset {
    Patched(CachedPatch),
    Unchanged,
}

struct CachedPatch {
    status: i64,
    headers: Vec<Value>,
    body_base64: String,
}

struct StoredPatch {
    status: i64,
    headers: Vec<Value>,
    body: String,
}

#[derive(Serialize, Deserialize)]
struct CacheMeta {
    #[serde(default)]
    unchanged: bool,
    #[serde(default)]
    status: i64,
    #[serde(default)]
    headers: Vec<Value>,
    #[serde(default)]
    length: u64,
}

impl PatchCache {
    fn new(dir: PathBuf, install_script: &str) -> Self {
        let digest = Sha256::digest(install_script.as_bytes());
        let tag = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Self { dir, tag }
    }

    fn entry_name(&self, url: &str) -> Option<String> {
        let name = url.split(['?', '#']).next()?.rsplit('/').next()?;
        cacheable_asset_name(name).then(|| format!("{}-{name}", self.tag))
    }

    async fn load(&self, url: &str) -> Option<CachedAsset> {
        let name = self.entry_name(url)?;
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || load_entry(&dir, &name))
            .await
            .ok()
            .flatten()
    }

    async fn store(&self, url: &str, patch: Option<StoredPatch>) {
        let Some(name) = self.entry_name(url) else {
            return;
        };
        let cache = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(error) = store_entry(&cache.dir, &name, patch) {
                log_service_event(
                    "renderer_asset_patch.cache_write_failed",
                    json!({ "entry": name, "message": format!("{error:#}") }),
                );
            }
            cache.prune();
        })
        .await;
    }

    fn prune(&self) {
        self.prune_to(MAX_CACHE_BYTES);
    }

    /// 删掉其他补丁版本的条目，并按最近使用时间把总量压到上限以内。
    fn prune_to(&self, max_bytes: u64) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let prefix = format!("{}-", self.tag);
        let mut current = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let Some(base) = name.strip_suffix(".meta.json") else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let body_size = std::fs::metadata(self.dir.join(base)).map_or(0, |body| body.len());
            let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            current.push((used, base.to_string(), meta.len() + body_size));
        }
        current.sort_by_key(|(used, _, _)| std::cmp::Reverse(*used));
        let mut total = 0;
        for (_, base, size) in current {
            total += size;
            if total > max_bytes {
                let _ = std::fs::remove_file(self.dir.join(format!("{base}.meta.json")));
                let _ = std::fs::remove_file(self.dir.join(&base));
            }
        }
    }
}

/// 形如 `app-initial-90eb42395b7d.js`：文件名里有一段至少 8 位的十六进制哈希。
fn cacheable_asset_name(name: &str) -> bool {
    let Some(stem) = [".js", ".mjs", ".cjs"]
        .iter()
        .find_map(|extension| name.strip_suffix(extension))
    else {
        return false;
    };
    name.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~' | b'.'))
        && !name.starts_with('.')
        && stem.split(['-', '~', '.']).skip(1).any(|segment| {
            segment.len() >= 8 && segment.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn load_entry(dir: &Path, name: &str) -> Option<CachedAsset> {
    let meta_path = dir.join(format!("{name}.meta.json"));
    let meta: CacheMeta = serde_json::from_slice(&std::fs::read(&meta_path).ok()?).ok()?;
    // 修改时间充当最近使用时间，供 prune 淘汰。
    if let Ok(file) = std::fs::File::options().append(true).open(&meta_path) {
        let _ = file.set_modified(SystemTime::now());
    }
    if meta.unchanged {
        return Some(CachedAsset::Unchanged);
    }
    let body = std::fs::read(dir.join(name)).ok()?;
    if body.len() as u64 != meta.length {
        return None;
    }
    Some(CachedAsset::Patched(CachedPatch {
        status: meta.status,
        headers: meta.headers,
        body_base64: base64::engine::general_purpose::STANDARD.encode(body),
    }))
}

fn store_entry(dir: &Path, name: &str, patch: Option<StoredPatch>) -> Result<()> {
    std::fs::create_dir_all(dir).context("创建补丁缓存目录失败")?;
    let meta = match patch {
        Some(patch) => {
            write_atomically(&dir.join(name), patch.body.as_bytes())?;
            CacheMeta {
                unchanged: false,
                status: patch.status,
                headers: patch.headers,
                length: patch.body.len() as u64,
            }
        }
        None => CacheMeta {
            unchanged: true,
            status: 0,
            headers: Vec::new(),
            length: 0,
        },
    };
    write_atomically(
        &dir.join(format!("{name}.meta.json")),
        &serde_json::to_vec(&meta).context("补丁缓存元数据序列化失败")?,
    )
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".tmp-{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, contents)
        .with_context(|| format!("写入补丁缓存 {} 失败", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| {
        let _ = std::fs::remove_file(&temporary);
        format!("替换补丁缓存 {} 失败", path.display())
    })
}

fn record_patch_log(payload: &str) {
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
    log_service_event(
        "renderer_asset_patch.request_skipped",
        json!({ "url": url, "reason": reason }),
    );
}

fn log_service_event(event: &str, detail: Value) {
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(event, detail);
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

    type TestSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    struct TestService {
        socket: TestSocket,
        context: Arc<ServiceContext>,
        client: tokio::task::JoinHandle<Result<()>>,
    }

    impl TestService {
        async fn start(cache_dir: &Path) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let context = Arc::new(ServiceContext::new(true, cache_dir.to_path_buf()));
            let client = tokio::spawn({
                let context = context.clone();
                async move {
                    let url = format!("ws://{address}");
                    let socket = codey_runtime_core::bridge::connect_cdp_websocket(&url)
                        .await
                        .unwrap();
                    serve(socket, context).await
                }
            });
            let (stream, _) = listener.accept().await.unwrap();
            let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            Self {
                socket,
                context,
                client,
            }
        }

        async fn recv(&mut self) -> Value {
            loop {
                let message = tokio::time::timeout(Duration::from_secs(2), self.socket.next())
                    .await
                    .expect("service should send a command")
                    .unwrap()
                    .unwrap();
                if let Message::Text(text) = message {
                    return serde_json::from_str(&text).unwrap();
                }
            }
        }

        async fn expect(&mut self, method: &str) -> Value {
            let command = self.recv().await;
            assert_eq!(command["method"], method, "{command}");
            command
        }

        async fn send(&mut self, payload: Value) {
            self.socket
                .send(Message::Text(payload.to_string().into()))
                .await
                .unwrap();
        }

        async fn reply(&mut self, command: &Value, result: Value) {
            self.send(json!({ "id": command["id"], "result": result }))
                .await;
        }

        async fn answer_startup(&mut self, targets: Value) {
            let auto_attach = self.expect("Target.setAutoAttach").await;
            assert_eq!(auto_attach["params"]["waitForDebuggerOnStart"], true);
            self.reply(&auto_attach, json!({})).await;
            let listed = self.expect("Target.getTargets").await;
            self.reply(&listed, json!({ "targetInfos": targets })).await;
        }

        async fn attach_page(&mut self, session: &str, target: &str, url: &str, waiting: bool) {
            self.send(json!({ "method": "Target.attachedToTarget", "params": {
                "sessionId": session, "waitingForDebugger": waiting,
                "targetInfo": { "targetId": target, "type": "page", "url": url }
            }}))
            .await;
            let fetch = self.expect("Fetch.enable").await;
            assert_eq!(fetch["sessionId"], session);
            self.reply(&fetch, json!({})).await;
        }

        async fn finish(mut self) {
            self.socket.close(None).await.unwrap();
            while let Some(Ok(_)) = self.socket.next().await {}
            self.client.await.unwrap().unwrap();
        }
    }

    fn paused(session: &str, request_id: &str, url: &str, response: bool) -> Value {
        let mut params = json!({ "requestId": request_id, "request": { "url": url } });
        if response {
            params["responseStatusCode"] = json!(200);
            params["responseHeaders"] = json!([
                { "name": "Content-Type", "value": "text/javascript" },
                { "name": "Content-Length", "value": "3" },
            ]);
        }
        json!({ "method": "Fetch.requestPaused", "sessionId": session, "params": params })
    }

    #[tokio::test]
    async fn new_pages_resume_only_after_interception_is_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let mut service = TestService::start(temp.path()).await;
        service.answer_startup(json!([])).await;
        service
            .send(json!({ "method": "Target.attachedToTarget", "params": {
                "sessionId": "worker-session", "waitingForDebugger": true,
                "targetInfo": { "targetId": "worker", "type": "service_worker", "url": "" }
            }}))
            .await;
        let resume_worker = service.expect("Runtime.runIfWaitingForDebugger").await;
        assert_eq!(resume_worker["sessionId"], "worker-session");
        let detach = service.expect("Target.detachFromTarget").await;
        assert_eq!(detach["params"]["sessionId"], "worker-session");

        service
            .send(json!({ "method": "Target.attachedToTarget", "params": {
                "sessionId": "page-session", "waitingForDebugger": true,
                "targetInfo": { "targetId": "main", "type": "page", "url": "" }
            }}))
            .await;
        let fetch = service.expect("Fetch.enable").await;
        assert_eq!(fetch["sessionId"], "page-session");
        let patterns = fetch["params"]["patterns"].as_array().unwrap();
        assert!(patterns.iter().all(|pattern| {
            pattern["resourceType"] == "Script"
                && pattern["requestStage"] == "Request"
                && pattern["urlPattern"]
                    .as_str()
                    .unwrap()
                    .starts_with("app://*/assets/")
        }));
        // 拦截落位前不能放行。
        assert!(
            tokio::time::timeout(Duration::from_millis(100), service.socket.next())
                .await
                .is_err()
        );
        service.reply(&fetch, json!({})).await;
        let resume = service.expect("Runtime.runIfWaitingForDebugger").await;
        assert_eq!(resume["sessionId"], "page-session");
        assert!(
            service
                .context
                .page_stats("main")
                .unwrap()
                .attached_before_load
        );
        service.finish().await;
    }

    #[tokio::test]
    async fn existing_pages_are_attached_explicitly() {
        let temp = tempfile::tempdir().unwrap();
        let mut service = TestService::start(temp.path()).await;
        service
            .answer_startup(json!([
                { "targetId": "main", "type": "page", "url": "about:blank" },
                { "targetId": "sandbox", "type": "webview", "url": "codex-sandbox://x" },
            ]))
            .await;
        let attach = service.expect("Target.attachToTarget").await;
        assert_eq!(
            attach["params"],
            json!({ "targetId": "main", "flatten": true })
        );
        service.reply(&attach, json!({ "sessionId": "s1" })).await;
        // 已加载完的页面也挂上（后续按需加载的脚本仍可改写），但记为未覆盖首个文档。
        service
            .attach_page("s2", "loaded", "app://-/index.html", false)
            .await;
        let stats = service.context.page_stats("loaded").unwrap();
        assert!(!stats.attached_before_load);
        service.finish().await;
    }

    #[tokio::test]
    async fn misses_patch_in_the_page_and_hits_fulfill_from_cache() {
        let temp = tempfile::tempdir().unwrap();
        let mut service = TestService::start(temp.path()).await;
        service.answer_startup(json!([])).await;
        service
            .attach_page("s1", "main", "about:blank", false)
            .await;

        let url = "app://-/assets/app-primary-b4ce9e46e8cf.js";
        service.send(paused("s1", "r1", url, false)).await;
        let intercept = service.expect("Fetch.continueRequest").await;
        assert_eq!(intercept["params"]["interceptResponse"], true);
        service.reply(&intercept, json!({})).await;

        service.send(paused("s1", "r1", url, true)).await;
        let body = service.expect("Fetch.getResponseBody").await;
        let encoded = base64::engine::general_purpose::STANDARD.encode("old");
        service
            .reply(&body, json!({ "body": encoded, "base64Encoded": true }))
            .await;
        let evaluate = service.expect("Runtime.evaluate").await;
        assert_eq!(evaluate["sessionId"], "s1");
        let expression = evaluate["params"]["expression"].as_str().unwrap();
        assert!(expression.contains("__codeyRendererPatchEntry"));
        assert!(expression.contains("\"old\""));
        service
            .reply(
                &evaluate,
                json!({ "result": { "value": { "installed": true, "patched": "new()", "logs": [] } } }),
            )
            .await;
        let fulfill = service.expect("Fetch.fulfillRequest").await;
        assert_eq!(
            fulfill["params"]["body"],
            base64::engine::general_purpose::STANDARD.encode("new()")
        );
        assert_eq!(
            fulfill["params"]["responseHeaders"],
            json!([{ "name": "Content-Type", "value": "text/javascript" }])
        );
        service.reply(&fulfill, json!({})).await;

        // 另一个窗口加载同一资产：请求阶段直接用缓存，不再读正文或求值。缓存
        // 写入是异步的，尚未落盘时会先要求响应阶段暂停。
        let mut cached = None;
        for attempt in 0..50 {
            let request_id = format!("again-{attempt}");
            service.send(paused("s1", &request_id, url, false)).await;
            let command = service.recv().await;
            if command["method"] == "Fetch.fulfillRequest" {
                cached = Some(command);
                break;
            }
            assert_eq!(command["method"], "Fetch.continueRequest");
            service.reply(&command, json!({})).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let cached = cached.expect("the patched asset should be served from the cache");
        assert_eq!(cached["params"]["body"], fulfill["params"]["body"]);
        assert_eq!(cached["params"]["responseCode"], 200);
        assert_eq!(
            cached["params"]["responseHeaders"],
            fulfill["params"]["responseHeaders"]
        );
        service.reply(&cached, json!({})).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stats = service.context.page_stats("main").unwrap();
        assert_eq!((stats.patched, stats.cache_hits), (2, 1));
        service.finish().await;
    }

    #[tokio::test]
    async fn failed_evaluation_passes_the_original_response_through_uncached() {
        let temp = tempfile::tempdir().unwrap();
        let mut service = TestService::start(temp.path()).await;
        service.answer_startup(json!([])).await;
        service.attach_page("s1", "main", "", true).await;
        let _ = service.expect("Runtime.runIfWaitingForDebugger").await;
        let url = "app://-/assets/app-initial-90eb42395b7d.js";
        service.send(paused("s1", "r1", url, true)).await;
        let body = service.expect("Fetch.getResponseBody").await;
        service.reply(&body, json!({ "body": "old" })).await;
        let evaluate = service.expect("Runtime.evaluate").await;
        service
            .reply(
                &evaluate,
                json!({ "result": { "value": { "installed": false, "logs": [] } } }),
            )
            .await;
        let pass = service.expect("Fetch.continueRequest").await;
        assert_eq!(pass["params"], json!({ "requestId": "r1" }));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(service.context.page_stats("main").unwrap().skipped, 1);
        assert!(service.context.cache.load(url).await.is_none());
        service.finish().await;
    }

    #[test]
    fn interception_covers_only_named_scripts() {
        let urls = |patterns: Value| -> Vec<String> {
            patterns
                .as_array()
                .unwrap()
                .iter()
                .map(|pattern| pattern["urlPattern"].as_str().unwrap().to_string())
                .collect()
        };
        let with_pet = urls(fetch_patterns(true));
        let without_pet = urls(fetch_patterns(false));
        assert!(with_pet.contains(&"app://*/assets/app-initial*".to_string()));
        assert!(with_pet.contains(&"app://*/assets/pets-settings*".to_string()));
        assert!(!without_pet.iter().any(|url| url.contains("pets-settings")));
        assert_eq!(with_pet.len(), without_pet.len() + PET_ASSET_PREFIXES.len());
    }

    #[test]
    fn page_patch_names_match_the_interception_list() {
        // 页面入口与 Fetch 名单必须一致：只拦不改浪费一次往返，只改不拦则补丁
        // 永远不会生效。
        let named = format!(
            "(?:(?:{})(?:[~-][^/]*)?)",
            PATCHABLE_ASSET_PREFIXES.join("|")
        );
        let pet = format!("(?:(?:{})(?:[~-][^/]*)?)", PET_ASSET_PREFIXES.join("|"));
        assert!(RENDERER_ASSET_PATCH_SCRIPT.contains(&named), "{named}");
        assert!(RENDERER_ASSET_PATCH_SCRIPT.contains(&pet), "{pet}");
    }

    #[test]
    fn only_content_hashed_assets_are_cacheable() {
        for name in [
            "app-initial-90eb42395b7d.js",
            "app-primary~b4ce9e46.mjs",
            "general-settings-679bf39c78a4.js",
        ] {
            assert!(cacheable_asset_name(name), "{name}");
        }
        for name in [
            "app-initial.js",
            "app-initial-dev.js",
            "app-initial-90eb4239.css",
            "..-90eb42395b7d.js",
            "app initial-90eb42395b7d.js",
        ] {
            assert!(!cacheable_asset_name(name), "{name}");
        }
        let cache = PatchCache::new(PathBuf::from("/tmp"), "script");
        assert_eq!(
            cache.entry_name("app://-/assets/app-initial-90eb42395b7d.js?build=1#x"),
            Some(format!("{}-app-initial-90eb42395b7d.js", cache.tag))
        );
    }

    #[tokio::test]
    async fn cache_round_trips_and_rejects_truncated_bodies() {
        let temp = tempfile::tempdir().unwrap();
        let cache = PatchCache::new(temp.path().to_path_buf(), "script");
        let patched = "app://-/assets/app-shared-52443cbe9ac4.js";
        let unchanged = "app://-/assets/general-settings-20eab868ec75.js";
        let patch = StoredPatch {
            status: 200,
            headers: vec![json!({ "name": "Content-Type", "value": "text/javascript" })],
            body: "patched()".to_string(),
        };
        cache.store(patched, Some(patch)).await;
        cache.store(unchanged, None).await;
        let Some(CachedAsset::Patched(hit)) = cache.load(patched).await else {
            panic!("patched entry should load");
        };
        assert_eq!(
            hit.body_base64,
            base64::engine::general_purpose::STANDARD.encode("patched()")
        );
        assert_eq!(hit.headers.len(), 1);
        assert!(matches!(
            cache.load(unchanged).await,
            Some(CachedAsset::Unchanged)
        ));
        // 另一个补丁版本看不到这些条目。
        let other = PatchCache::new(temp.path().to_path_buf(), "other script");
        assert!(other.load(patched).await.is_none());

        let body = temp.path().join(cache.entry_name(patched).unwrap());
        std::fs::write(&body, "patch").unwrap();
        assert!(cache.load(patched).await.is_none());
    }

    #[test]
    fn prune_drops_other_versions_and_least_recently_used_entries() {
        let temp = tempfile::tempdir().unwrap();
        let cache = PatchCache::new(temp.path().to_path_buf(), "script");
        std::fs::write(temp.path().join("stale-app-initial-90eb42395b7d.js"), "x").unwrap();
        let big = "x".repeat(400);
        for (index, name) in ["a-11111111.js", "b-22222222.js", "c-33333333.js"]
            .iter()
            .enumerate()
        {
            let entry = format!("{}-{name}", cache.tag);
            let patch = StoredPatch {
                status: 200,
                headers: Vec::new(),
                body: big.clone(),
            };
            store_entry(temp.path(), &entry, Some(patch)).unwrap();
            let used = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + index as u64);
            std::fs::File::options()
                .append(true)
                .open(temp.path().join(format!("{entry}.meta.json")))
                .unwrap()
                .set_modified(used)
                .unwrap();
        }
        cache.prune_to(600);
        let mut left: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let tag = &cache.tag;
        assert_eq!(
            left,
            [
                format!("{tag}-c-33333333.js"),
                format!("{tag}-c-33333333.js.meta.json"),
            ]
        );
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
