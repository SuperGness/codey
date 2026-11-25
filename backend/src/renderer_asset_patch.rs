//! 渲染资产补丁服务：一条浏览器级 CDP 连接覆盖 Codex 的全部页面。
//!
//! Electron fuse 关闭后主进程 `protocol.handle` 改写不可用，这里改用 Fetch 域
//! 改写 `app://` 脚本响应。新页面经自动挂载在创建时暂停，Fetch 拦截落位后才
//! 放行；连接前已存在的页面直接挂上，只要尚未导航就能覆盖首个文档。拦截只匹配
//! 补丁名单里的语义脚本和带内容哈希的数字 chunk；正文不含补丁前置规则的
//! 资产不送进页面求值，只记为无需改写。
//!
//! 补丁入口是纯函数，结果只取决于补丁脚本与资产内容，而 Codex 的资产文件名
//! 自带内容哈希：结果按「补丁脚本版本 + 文件名」缓存到磁盘，命中时在请求阶段
//! 直接 fulfill；未命中才取响应正文，在请求它的页面里跑一次补丁入口。错过首个
//! 文档的页面不强制重载，只在诊断里记为未覆盖。

mod anchors;
mod cache;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::error_log;
pub(crate) use anchors::PatchAnchorMatcher;
use cache::{CACHE_DIR_NAME, CachedAsset, PatchCache, StoredPatch};

/// 补丁脚本注入页面后暴露的纯函数入口：`(url, source) => patched | null`。
const RENDERER_ASSET_PATCH_SCRIPT: &str = include_str!("renderer_asset_patch.js");
/// CDP Fetch 的通配符不支持字符类；数字 chunk 先宽匹配，再在请求处理时严格筛选。
const NUMERIC_ASSET_URL_PATTERN: &str = "app://*/assets/?*.js*";
/// 带内容哈希的样式表资源；扫描未完成时宽匹配，请求处理时按哈希名严格筛选。
const CSS_ASSET_URL_PATTERN: &str = "app://*/assets/*.css*";
/// 宠物精灵图。rspack 构建保留可读文件名，按宠物 id 前缀识别；只在关闭宠物时拦截。
const PET_SPRITE_URL_PATTERN: &str = "app://*/assets/*-spritesheet-v*.webp*";
/// 未选择过宠物时 Codex 使用的内置形象。
const DEFAULT_AVATAR_ID: &str = "codex";
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

static NEXT_MESSAGE_ID: AtomicU64 = AtomicU64::new(1);
static SERVICE: OnceLock<Mutex<Option<ServiceHandle>>> = OnceLock::new();

fn next_message_id() -> u64 {
    NEXT_MESSAGE_ID.fetch_add(1, Ordering::Relaxed)
}

fn service_slot() -> &'static Mutex<Option<ServiceHandle>> {
    SERVICE.get_or_init(|| Mutex::new(None))
}

/// 补丁脚本注入页面用的完整脚本。
pub(crate) fn renderer_asset_patch_document_script() -> &'static str {
    RENDERER_ASSET_PATCH_SCRIPT
}

struct ServiceHandle {
    debug_port: u16,
    disable_pet: bool,
    paths: Option<CodexPaths>,
    context: Arc<ServiceContext>,
    task: tokio::task::JoinHandle<()>,
}

/// 本次启动的 Codex 安装目录与数据目录：前者用于离线扫描前端资源，后者用于
/// 读取当前选中的宠物。
#[derive(Clone, Debug)]
pub(crate) struct CodexPaths {
    pub(crate) app_dir: PathBuf,
    pub(crate) codex_home: PathBuf,
}

/// Codex 启动前调用：替换旧服务，从端点一出现就开始挂载页面。
pub(crate) fn start_service(debug_port: u16, disable_pet: bool, paths: Option<CodexPaths>) {
    let mut context = ServiceContext::new(disable_pet, default_cache_dir());
    if let Some(paths) = &paths {
        context.start_asset_scan(paths);
    }
    let context = Arc::new(context);
    let task = tokio::spawn(run_service(debug_port, context.clone()));
    let previous = service_slot()
        .lock()
        .expect("patch service slot poisoned")
        .replace(ServiceHandle {
            debug_port,
            disable_pet,
            paths,
            context,
            task,
        });
    if let Some(previous) = previous {
        previous.task.abort();
    }
}

/// 注入路径调用：同一端口的服务仍在运行时什么都不做；重启时沿用上次启动
/// 登记的 Codex 路径。
pub(crate) fn ensure_service(debug_port: u16, disable_pet: bool) {
    let (running, paths) = {
        let slot = service_slot().lock().expect("patch service slot poisoned");
        let service = slot.as_ref();
        (
            service.is_some_and(|service| {
                service.debug_port == debug_port
                    && service.disable_pet == disable_pet
                    && !service.task.is_finished()
            }),
            service.and_then(|service| service.paths.clone()),
        )
    };
    if !running {
        start_service(debug_port, disable_pet, paths);
    }
}

/// 某个页面的拦截统计；服务未挂上该页面时返回 None。
pub(crate) fn page_stats(target_id: &str) -> Option<PageStats> {
    let slot = service_slot().lock().expect("patch service slot poisoned");
    slot.as_ref()?.context.page_stats(target_id)
}

/// `attachedBeforeLoad` 为 false 且 `patched` 为 0 说明挂上时首个文档已加载，
/// 本次运行的主 bundle 没有补丁。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageStats {
    pub(crate) attached_before_load: bool,
    pub(crate) intercepted: u64,
    pub(crate) patched: u64,
    pub(crate) cache_hits: u64,
    pub(crate) skipped: u64,
    /// 关闭宠物时拦下的未选中宠物精灵图。
    pub(crate) blocked: u64,
}

struct PageEntry {
    target_id: String,
    stats: PageStats,
}

type PendingAssetScan = tokio::task::JoinHandle<Result<crate::renderer_asset_scan::ScanOutcome>>;

struct CachedSelectedPet {
    home: PathBuf,
    stamp: crate::pet_slim_patch::GlobalStateStamp,
    pet_id: String,
}

struct ServiceContext {
    install_script: Arc<str>,
    /// 当前拦截规则。离线扫描完成后换成精确地址，并下发给已挂上的页面。
    patterns: Mutex<Value>,
    disable_pet: bool,
    /// Codex 数据目录；读取当前选中的宠物。
    codex_home: Option<PathBuf>,
    /// 按全局状态文件元数据缓存的选中宠物 id，避免每次拦截精灵图重复读盘解析。
    selected_pet: tokio::sync::Mutex<Option<CachedSelectedPet>>,
    /// 与连接调试端点并行进行的资源扫描，服务开始时取走。
    pending_scan: Mutex<Option<PendingAssetScan>>,
    /// 页面补丁的复合前置规则；资源一条都不命中时不必送进页面求值。
    matcher: PatchAnchorMatcher,
    cache: PatchCache,
    /// 按扁平化会话 id 记录已挂上的页面。
    pages: Mutex<HashMap<String, PageEntry>>,
    permits: tokio::sync::Semaphore,
}

impl ServiceContext {
    fn new(disable_pet: bool, cache_dir: PathBuf) -> Self {
        let install_script: Arc<str> = Arc::from(renderer_asset_patch_document_script());
        Self {
            cache: PatchCache::new(cache_dir, &install_script),
            install_script,
            patterns: Mutex::new(fetch_patterns(disable_pet, None)),
            disable_pet,
            codex_home: None,
            selected_pet: tokio::sync::Mutex::new(None),
            pending_scan: Mutex::new(None),
            matcher: PatchAnchorMatcher::for_script(RENDERER_ASSET_PATCH_SCRIPT)
                .expect("renderer_asset_patch.js 内置复合前置规则表无效"),
            pages: Mutex::new(HashMap::new()),
            permits: tokio::sync::Semaphore::new(MAX_CONCURRENT_PATCHES),
        }
    }

    fn start_asset_scan(&mut self, paths: &CodexPaths) {
        self.codex_home = Some(paths.codex_home.clone());
        let app_dir = paths.app_dir.clone();
        let cache_file = self
            .cache
            .dir
            .join(format!("{}-asset-scan.json", self.cache.tag));
        let matcher = self.matcher.clone();
        *self
            .pending_scan
            .get_mut()
            .expect("asset scan slot poisoned") = Some(tokio::task::spawn_blocking(move || {
            crate::renderer_asset_scan::load_or_scan_with_matcher(&app_dir, &cache_file, &matcher)
        }));
    }

    fn current_patterns(&self) -> Value {
        self.patterns
            .lock()
            .expect("patch pattern slot poisoned")
            .clone()
    }

    /// 采用扫描结果；扫描失败时保留通配拦截，补丁照常生效。
    fn apply_asset_scan(
        &self,
        outcome: std::result::Result<
            Result<crate::renderer_asset_scan::ScanOutcome>,
            tokio::task::JoinError,
        >,
        started: std::time::Instant,
    ) -> bool {
        let outcome = match outcome {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => {
                log_service_event(
                    "renderer_asset_patch.asset_scan_failed",
                    json!({ "message": format!("{error:#}") }),
                );
                return false;
            }
            Err(error) => {
                log_service_event(
                    "renderer_asset_patch.asset_scan_failed",
                    json!({ "message": error.to_string() }),
                );
                return false;
            }
        };
        if let Some(message) = &outcome.cache_write_error {
            log_service_event(
                "renderer_asset_patch.asset_scan_cache_write_failed",
                json!({ "message": message }),
            );
        }
        log_service_event(
            "renderer_asset_patch.asset_scan",
            json!({
                "cached": outcome.cached,
                "numericChunks": outcome.scan.numeric_chunks,
                "candidates": outcome.scan.candidates.len(),
                "waitedMs": started.elapsed().as_millis() as u64,
            }),
        );
        *self.patterns.lock().expect("patch pattern slot poisoned") =
            fetch_patterns(self.disable_pet, Some(&outcome.scan));
        true
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

/// 只在请求阶段拦截可改写的数字 chunk 与带哈希样式表；有扫描结果时只拦截精确候选地址，
/// 否则按通配地址拦截再在正文上预筛。开启宠物精简时额外拦截宠物精灵图。
fn fetch_patterns(
    disable_pet: bool,
    scan: Option<&crate::renderer_asset_scan::AssetScan>,
) -> Value {
    let mut patterns: Vec<Value> = match scan {
        Some(scan) => scan
            .candidates
            .iter()
            .map(|path| {
                let resource_type = if path.ends_with(".css") {
                    "Stylesheet"
                } else {
                    "Script"
                };
                json!({
                    "urlPattern": format!("app://*/assets/{path}*"),
                    "resourceType": resource_type,
                    "requestStage": "Request",
                })
            })
            .collect(),
        None => vec![
            json!({
                "urlPattern": NUMERIC_ASSET_URL_PATTERN,
                "resourceType": "Script",
                "requestStage": "Request",
            }),
            json!({
                "urlPattern": CSS_ASSET_URL_PATTERN,
                "resourceType": "Stylesheet",
                "requestStage": "Request",
            }),
        ],
    };
    if disable_pet {
        // Codex 以图片元素解码精灵图。
        patterns.push(json!({
            "urlPattern": PET_SPRITE_URL_PATTERN,
            "resourceType": "Image",
            "requestStage": "Request",
        }));
    }
    Value::Array(patterns)
}

/// 内置宠物精灵图的宠物 id，如 `null-signal-spritesheet-v7.e7af0b42f5.webp` 为
/// `null-signal`；不是精灵图时返回 None。
fn pet_sprite_id(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    if url.scheme() != "app" {
        return None;
    }
    let name = url.path().strip_prefix("/assets/")?;
    let stem = name.strip_suffix(".webp")?;
    let (pet, version) = stem.rsplit_once("-spritesheet-v")?;
    let (number, hash) = version.split_once('.')?;
    let valid_pet = !pet.is_empty()
        && pet
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    (valid_pet
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && (8..=32).contains(&hash.len())
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| pet.to_string())
}

/// 当前选中的内置宠物；读不到设置时按 Codex 默认形象处理。自定义宠物使用
/// 自己的精灵图地址，不会与任何内置精灵图同名。
async fn selected_pet_id(context: &ServiceContext) -> String {
    let Some(home) = context.codex_home.clone() else {
        return DEFAULT_AVATAR_ID.to_string();
    };
    let mut cached = context.selected_pet.lock().await;
    let previous = cached.take();
    match tokio::task::spawn_blocking(move || -> (Option<CachedSelectedPet>, String) {
        let stamp = crate::pet_slim_patch::global_state_stamp(&home);
        if let Some(entry) = previous
            && entry.home == home
            && entry.stamp == stamp
        {
            let pet_id = entry.pet_id.clone();
            return (Some(entry), pet_id);
        }
        match crate::pet_slim_patch::selected_avatar_id(&home) {
            Ok(selected) => {
                let pet_id = selected.unwrap_or_else(|| DEFAULT_AVATAR_ID.to_string());
                (
                    Some(CachedSelectedPet {
                        home,
                        stamp,
                        pet_id: pet_id.clone(),
                    }),
                    pet_id,
                )
            }
            Err(error) => {
                log_service_event(
                    "renderer_asset_patch.selected_pet_unreadable",
                    json!({ "message": format!("{error:#}") }),
                );
                (None, DEFAULT_AVATAR_ID.to_string())
            }
        }
    })
    .await
    {
        Ok((next_cache, pet_id)) => {
            *cached = next_cache;
            pet_id
        }
        Err(_) => DEFAULT_AVATAR_ID.to_string(),
    }
}

/// 识别可改写的资产 URL：根资源目录或 async 子目录下带内容哈希的数字 chunk JS 与样式表。
fn is_patchable_asset_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if url.scheme() != "app" {
        return false;
    }
    let path = url.path();
    let Some(name) = path
        .strip_prefix("/assets/async/")
        .or_else(|| path.strip_prefix("/assets/"))
    else {
        return false;
    };
    numeric_asset_name(name) || css_asset_name(name)
}

pub(crate) fn numeric_asset_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".js") else {
        return false;
    };
    let Some((id, hash)) = stem.split_once('.') else {
        return false;
    };
    !id.is_empty()
        && id.len() <= 10
        && id.bytes().all(|byte| byte.is_ascii_digit())
        && (8..=32).contains(&hash.len())
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn css_asset_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".css") else {
        return false;
    };
    !stem.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~' | b'.'))
        && stem.split(['-', '~', '.']).any(|segment| {
            (8..=32).contains(&segment.len())
                && segment.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
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
        let mut payload = serde_json::Map::new();
        payload.insert("id".to_string(), Value::from(id));
        payload.insert("method".to_string(), Value::String(method.to_string()));
        payload.insert("params".to_string(), params);
        if let Some(session_id) = session_id {
            payload.insert(
                "sessionId".to_string(),
                Value::String(session_id.to_string()),
            );
        }
        Message::Text(Value::Object(payload).to_string().into())
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
    // 资源扫描与连接调试端点并行进行。已完成时（通常是读到缓存）在挂载页面
    // 前换上精确规则；未完成时先按通配规则挂载，完成后再更新各页面。
    let pending_scan = context
        .pending_scan
        .lock()
        .expect("asset scan slot poisoned")
        .take();
    if let Some(scan) = pending_scan {
        let started = std::time::Instant::now();
        if scan.is_finished() {
            context.apply_asset_scan(scan.await, started);
        } else {
            let (connection, context) = (connection.clone(), context.clone());
            tasks.spawn(async move {
                if context.apply_asset_scan(scan.await, started) {
                    refresh_fetch_patterns(&connection, &context).await;
                }
            });
        }
    }
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

/// 把当前拦截规则下发给已挂上的页面；重复 Fetch.enable 会替换原有规则。
async fn refresh_fetch_patterns(connection: &BrowserConnection, context: &ServiceContext) {
    let patterns = context.current_patterns();
    let sessions: Vec<String> = context
        .pages
        .lock()
        .expect("patch page registry poisoned")
        .keys()
        .cloned()
        .collect();
    for session in sessions {
        if let Err(error) = connection
            .call(
                "Fetch.enable",
                json!({ "patterns": patterns }),
                Some(session.as_str()),
            )
            .await
        {
            log_service_event(
                "renderer_asset_patch.fetch_refresh_failed",
                json!({ "message": format!("{error:#}") }),
            );
        }
    }
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
            json!({ "patterns": context.current_patterns() }),
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
    if context.disable_pet
        && let Some(pet) = pet_sprite_id(url)
    {
        // 关闭宠物时只放行当前选中宠物的精灵图：语音浮窗仍会显示它，其余
        // 每张解码后约 13 MB，主要由设置页的选择网格一次性加载。
        if pet == selected_pet_id(&context).await {
            pass_through();
        } else {
            connection.notify(
                "Fetch.failRequest",
                json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
                session,
            );
            context.update_page(&session_id, |stats| stats.blocked += 1);
        }
        return;
    }
    if !is_patchable_asset_url(url) {
        pass_through();
        return;
    }
    let response_stage =
        params.get("responseStatusCode").is_some() || params.get("responseErrorReason").is_some();
    if !response_stage {
        context.update_page(&session_id, |stats| stats.intercepted += 1);
        // 已确认无需改写的资产在内存索引直通，免去竞争补丁并发许可与读盘。
        if context.cache.is_known_unchanged(url) {
            context.update_page(&session_id, |stats| stats.cache_hits += 1);
            pass_through();
            return;
        }
        let Ok(permit) = context.permits.acquire().await else {
            pass_through();
            return;
        };
        match context.cache.load(url).await {
            Some(CachedAsset::Patched(asset)) => {
                let mut params = json!({
                    "requestId": request_id,
                    "responseCode": asset.status,
                    "responseHeaders": asset.headers,
                });
                params["body"] = Value::String(asset.body_base64);
                let fulfilled = connection
                    .call("Fetch.fulfillRequest", params, session)
                    .await;
                match fulfilled {
                    Ok(_) => {
                        context.update_page(&session_id, |stats| {
                            stats.patched += 1;
                            stats.cache_hits += 1;
                        });
                    }
                    Err(error) => {
                        log_patch_skip(url, format!("fulfill cached: {error:#}"));
                        context.update_page(&session_id, |stats| stats.skipped += 1);
                        pass_through();
                    }
                }
            }
            Some(CachedAsset::Unchanged) => {
                context.update_page(&session_id, |stats| stats.cache_hits += 1);
                drop(permit);
                pass_through();
            }
            None => {
                drop(permit);
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
            let mut params = json!({
                "requestId": request_id,
                "responseCode": status,
                "responseHeaders": headers,
            });
            params["body"] = Value::String(body_base64);
            let fulfilled = connection
                .call("Fetch.fulfillRequest", params, session)
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
    let source = take_response_source(body)?;
    // 数字 chunk 大多与补丁无关：先按复合前置规则预筛，免得逐个把源码连同补丁
    // 脚本送进页面求值。
    if !context.matcher.matches(&source) {
        return Ok(None);
    }
    let expression = patch_expression(&context.install_script, url, source)?;
    let mut params = serde_json::Map::new();
    params.insert("expression".to_string(), Value::String(expression));
    params.insert("returnByValue".to_string(), Value::Bool(true));
    let mut response = connection
        .call("Runtime.evaluate", Value::Object(params), session)
        .await?;
    if let Some(exception) = response["result"].get("exceptionDetails") {
        bail!("补丁求值抛出异常：{exception}");
    }
    let outcome_value = response
        .get_mut("result")
        .and_then(|result| result.get_mut("result"))
        .and_then(|result| result.get_mut("value"))
        .map(Value::take)
        .unwrap_or(Value::Null);
    drop(response);
    let outcome: PatchOutcome =
        serde_json::from_value(outcome_value).context("补丁求值结果格式无效")?;
    for record in &outcome.logs {
        record_patch_log(record);
    }
    if !outcome.installed {
        bail!("页面未能装上补丁入口");
    }
    Ok(outcome.patched)
}

fn take_response_source(mut body: Value) -> Result<String> {
    let base64_encoded = body
        .get("result")
        .and_then(|result| result.get("base64Encoded"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let raw = body
        .pointer_mut("/result/body")
        .and_then(|value| match value {
            Value::String(text) => Some(std::mem::take(text)),
            _ => None,
        })
        .unwrap_or_default();
    if !base64_encoded {
        return Ok(raw);
    }
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(raw)
            .context("响应正文 base64 解码失败")?,
    )
    .context("响应正文不是 UTF-8")
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
fn patch_expression(install_script: &str, url: &str, source: String) -> Result<String> {
    let url = serde_json::to_string(url).context("补丁 URL 序列化失败")?;
    let source = serde_json::to_string(&source).context("补丁源码序列化失败")?;
    Ok(format!(
        "(()=>{{const logs=[];const previous=globalThis.__codeyPatchLog;\
globalThis.__codeyPatchLog=(record)=>{{logs.push(String(record))}};\
try{{if(typeof globalThis.__codeyRendererPatchEntry!==\"function\"){{{install_script}\n}}\
const entry=globalThis.__codeyRendererPatchEntry;\
if(typeof entry!==\"function\")return{{installed:false,logs}};\
return{{installed:true,patched:entry({url},{source}),logs}}}}\
catch(error){{logs.push(String(error));return{{installed:false,logs}}}}\
finally{{if(previous===undefined)delete globalThis.__codeyPatchLog;else globalThis.__codeyPatchLog=previous}}}})()",
        url = url,
        source = source,
    ))
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
